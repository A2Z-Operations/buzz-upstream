part of 'relay_session.dart';

extension _RelaySessionEventAdmission on RelaySessionNotifier {
  void _handleEvent(List<dynamic> data) {
    if (data.length < 3 || data[1] is! String) return;
    final subId = data[1] as String;
    final connectionGeneration = _connectionGeneration;
    final contextGeneration = _contextGeneration;

    final historySub = _historySubscriptions[subId];
    if (historySub != null) {
      historySub.receivedEventFrames++;
      historySub.pendingVerifications++;
      final verification = _verifier.verify(data[2]);
      historySub.verificationTail = historySub.verificationTail.then((_) async {
        try {
          final event = await verification;
          if (_historySubscriptions[subId] != historySub ||
              connectionGeneration != _connectionGeneration ||
              contextGeneration != _contextGeneration) {
            return;
          }
          if (event != null) historySub.events.add(event);
        } catch (error, stackTrace) {
          if (_historySubscriptions[subId] == historySub &&
              connectionGeneration == _connectionGeneration &&
              contextGeneration == _contextGeneration) {
            historySub.verificationError ??= error;
            historySub.verificationStackTrace ??= stackTrace;
          }
        } finally {
          historySub.pendingVerifications--;
        }
      });
      return;
    }

    final liveSub = _liveSubscriptions[subId];
    if (liveSub != null) {
      final verificationEpoch = liveSub.verificationEpoch;
      liveSub.receivedEventFrames++;
      liveSub.pendingVerifications++;
      final verification = _verifier.verify(data[2]);
      liveSub.verificationTail = liveSub.verificationTail.then((_) async {
        try {
          final event = await verification;
          if (_liveSubscriptions[subId] != liveSub ||
              connectionGeneration != _connectionGeneration ||
              contextGeneration != _contextGeneration ||
              verificationEpoch != liveSub.verificationEpoch ||
              event == null) {
            return;
          }

          // Only a verified event can prove activity or advance replay state.
          _resetClosedRetry(liveSub);
          liveSub.validEventFrames++;
          if (liveSub.lastSeenCreatedAt == null ||
              event.createdAt > liveSub.lastSeenCreatedAt!) {
            liveSub.lastSeenCreatedAt = event.createdAt;
          }
          _eventBuffer.add(_BufferedEvent(subId, event));
          _scheduleFlush();
        } catch (error, stackTrace) {
          if (_liveSubscriptions[subId] == liveSub &&
              connectionGeneration == _connectionGeneration &&
              contextGeneration == _contextGeneration &&
              verificationEpoch == liveSub.verificationEpoch) {
            _failLiveVerification(subId, liveSub, error, stackTrace);
          }
        } finally {
          if (verificationEpoch == liveSub.verificationEpoch) {
            liveSub.pendingVerifications--;
          }
        }
      });
    }
  }

  void _handleEose(List<dynamic> data) {
    if (data.length < 2 || data[1] is! String) return;
    final subId = data[1] as String;

    final historySub = _historySubscriptions[subId];
    if (historySub != null) {
      if (historySub.pendingVerifications == 0) {
        _completeHistoryEose(subId, historySub);
      } else {
        unawaited(_completeHistoryEoseAfterVerification(subId, historySub));
      }
      return;
    }

    final liveSub = _liveSubscriptions[subId];
    if (liveSub != null) {
      final connectionGeneration = _connectionGeneration;
      final contextGeneration = _contextGeneration;
      final verificationEpoch = liveSub.verificationEpoch;
      if (liveSub.pendingVerifications == 0) {
        _completeLiveEose(
          subId,
          liveSub,
          connectionGeneration,
          contextGeneration,
          verificationEpoch,
        );
      } else {
        unawaited(
          _completeLiveEoseAfterVerification(
            subId,
            liveSub,
            connectionGeneration,
            contextGeneration,
            verificationEpoch,
          ),
        );
      }
    }
  }

  Future<void> _completeHistoryEoseAfterVerification(
    String subId,
    _HistorySubscription subscription,
  ) async {
    await subscription.verificationTail;
    _completeHistoryEose(subId, subscription);
  }

  void _completeHistoryEose(String subId, _HistorySubscription subscription) {
    if (_historySubscriptions[subId] != subscription) return;
    _historySubscriptions.remove(subId);
    subscription.timeout.cancel();
    final verificationError = subscription.verificationError;
    if (!subscription.completer.isCompleted) {
      if (verificationError != null) {
        subscription.completer.completeError(
          verificationError,
          subscription.verificationStackTrace,
        );
      } else if (subscription.receivedEventFrames > 0 &&
          subscription.events.isEmpty) {
        subscription.completer.completeError(
          const FormatException(
            'relay history returned no valid signed events',
          ),
        );
      } else {
        subscription.completer.complete(subscription.events);
      }
    }
    _sendClose(subId);
  }

  Future<void> _completeLiveEoseAfterVerification(
    String subId,
    _LiveSubscription subscription,
    int connectionGeneration,
    int contextGeneration,
    int verificationEpoch,
  ) async {
    await subscription.verificationTail;
    _completeLiveEose(
      subId,
      subscription,
      connectionGeneration,
      contextGeneration,
      verificationEpoch,
    );
  }

  void _completeLiveEose(
    String subId,
    _LiveSubscription subscription,
    int connectionGeneration,
    int contextGeneration,
    int verificationEpoch,
  ) {
    if (_liveSubscriptions[subId] != subscription ||
        connectionGeneration != _connectionGeneration ||
        contextGeneration != _contextGeneration ||
        verificationEpoch != subscription.verificationEpoch) {
      return;
    }
    if (subscription.receivedEventFrames > 0 &&
        subscription.validEventFrames == 0) {
      _failLiveVerification(
        subId,
        subscription,
        const FormatException(
          'relay subscription returned no valid signed events',
        ),
        StackTrace.current,
      );
      return;
    }

    subscription.receivedEventFrames = 0;
    subscription.validEventFrames = 0;
    // Flush replay callbacks before signaling ready. This ordering matters for
    // retry replays, whose original ready completer has already been released.
    _resetClosedRetry(subscription);
    _flushBufferedEventsNow();
    subscription.onStatusChanged?.call(RelaySubscriptionStatus.ready);
    final readyCompleter = subscription.readyCompleter;
    if (readyCompleter != null && !readyCompleter.isCompleted) {
      readyCompleter.complete();
      subscription.readyCompleter = null;
    }
  }

  void _failLiveVerification(
    String subId,
    _LiveSubscription subscription,
    Object error,
    StackTrace stackTrace,
  ) {
    if (_liveSubscriptions[subId] != subscription) return;
    final readyCompleter = subscription.readyCompleter;
    if (readyCompleter != null && !readyCompleter.isCompleted) {
      readyCompleter.completeError(error, stackTrace);
    } else {
      subscription.onClosed?.call('event verification failed: $error');
    }
    _removeLiveSubscription(subId, subscription);
  }

  void _retireLiveVerification() {
    for (final subscription in _liveSubscriptions.values) {
      _retireLiveSubscriptionVerification(subscription);
    }
  }

  void _retireLiveSubscriptionVerification(_LiveSubscription subscription) {
    subscription.verificationEpoch++;
    subscription.verificationTail = Future.value();
    subscription.pendingVerifications = 0;
    subscription.receivedEventFrames = 0;
    subscription.validEventFrames = 0;
  }
}
