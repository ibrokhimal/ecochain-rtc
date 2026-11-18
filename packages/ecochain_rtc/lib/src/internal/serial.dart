import 'dart:async';

/// Runs async operations one after another.
///
/// Media operations (enable camera, renegotiate, ICE restart) must not
/// interleave: two overlapping `createOffer` calls on one peer connection
/// fail, and two overlapping camera captures leak a camera. Callers queue
/// their work here instead of guarding every method with flags.
class Serial {
  Future<void> _tail = Future.value();

  /// Queues [action] and completes with its result.
  Future<T> run<T>(Future<T> Function() action) {
    final result = _tail.then((_) => action());
    _tail = result.then((_) {}, onError: (_) {});
    return result;
  }
}
