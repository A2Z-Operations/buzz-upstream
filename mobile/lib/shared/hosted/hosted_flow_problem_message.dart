import 'hosted_flow.dart';

String hostedFlowProblemMessage(
  HostedFlowProblem? problem,
) => switch (problem) {
  HostedFlowProblem.unsupportedEnvironment =>
    'This community needs a different or unsupported Builderlab environment. Use an app build configured for this community.',
  HostedFlowProblem.denied => 'This account does not have access.',
  HostedFlowProblem.differentAccount =>
    'Your browser signed in to a different account. Sign in to your original account on the web, then reopen setup. Your saved account and keys have been kept.',
  HostedFlowProblem.identityUnreadable =>
    'Your saved identity could not be read safely. Leave setup and recover access to this device’s secure storage before continuing.',
  HostedFlowProblem.backupUnavailable =>
    'Secure backup is not supported on this device yet. Your identity has been kept; joining requires a confirmed backup.',
  HostedFlowProblem.requestRejected =>
    'The account service could not accept this setup request. Your saved account and identity have been kept.',
  HostedFlowProblem.invalidResponse =>
    'The account service returned a response this app cannot safely use. Setup has stopped.',
  HostedFlowProblem.identityMismatch =>
    'This account uses a different identity. Your saved identity has been kept.',
  HostedFlowProblem.backupRequired =>
    'Your identity needs a confirmed backup before joining. Backup setup is not available in this build yet.',
  HostedFlowProblem.bindingRequired =>
    'Connect your identity from the account setup page first.',
  HostedFlowProblem.identityUnavailable =>
    'We could not safely recover an identity for this account. Your saved keys have been kept.',
  _ => 'We could not finish connecting. You can try again.',
};
