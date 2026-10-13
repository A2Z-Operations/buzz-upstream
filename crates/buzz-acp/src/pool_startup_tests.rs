//! Exercise pool admission with real ACP subprocesses. No relay or provider.
use super::*;

#[tokio::test]
async fn first_ready_worker_serves_before_the_remaining_workers_start() {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("adapter.py");
    let launches = temp.path().join("launches");
    std::fs::write(
        &script,
        r#"import json, os, sys
with open(os.environ['LAUNCHES'], 'a') as file:
    file.write('started\n')
for line in sys.stdin:
    message = json.loads(line)
    if message.get('method') == 'initialize':
        print(json.dumps({'jsonrpc': '2.0', 'id': message['id'],
                          'result': {'protocolVersion': 1,
                                     'agentInfo': {'name': 'startup-probe'}}}), flush=True)
"#,
    )
    .unwrap();
    let startup = PoolStartup {
        agents: 3,
        command: "python3".into(),
        args: vec![script.to_str().unwrap().into()],
        extra_env: vec![("LAUNCHES".into(), launches.to_str().unwrap().into())],
        has_generated_codex_config: false,
        model: None,
        effort_level: None,
        observer: None,
    };
    let mut pool = initialize_agent_pool(&startup, None).await.unwrap();
    let started = std::fs::read_to_string(&launches).unwrap();
    let live = pool.live_count();
    let ready = pool.any_idle();
    let remaining = (!pool.slot_alive(1), !pool.slot_alive(2));
    shutdown_agent_pool(&mut pool).await;
    assert_eq!(started.lines().count(), 1);
    assert_eq!(live, 1);
    assert!(ready, "the returned worker must be available for dispatch");
    assert_eq!(
        remaining,
        (true, true),
        "refill retains configured capacity"
    );
}
