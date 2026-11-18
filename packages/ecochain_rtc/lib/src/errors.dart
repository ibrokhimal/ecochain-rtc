/// Machine readable reasons why an [RtcException] was thrown.
///
/// Server side codes mirror the `code` field of the signaling protocol
/// (see `docs/PROTOCOL.md`); the others are raised by the SDK itself.
enum RtcErrorCode {
  /// The token is missing, malformed or signed with another secret.
  unauthorized,

  /// The token expired. Fetch a new one and connect again.
  tokenExpired,

  /// The token is valid but does not allow this action (for example a
  /// participant trying to end the meeting).
  forbidden,

  /// The room does not exist and the server does not create rooms on join.
  roomNotFound,

  /// The room reached its `max_participants` limit.
  roomFull,

  /// The host ended the meeting.
  roomEnded,

  /// The server rejected a request because of the current state.
  invalidState,

  /// A limit such as the maximum number of video subscriptions was hit.
  limitExceeded,

  /// The server is temporarily unavailable. Retrying later can help.
  unavailable,

  /// The URL is not a valid `wss://` (or allowed development `ws://`) URL.
  invalidUrl,

  /// The request did not complete in time.
  timeout,

  /// The operation needs a connected session.
  notConnected,

  /// The user or the OS denied access to the microphone, camera or screen.
  permissionDenied,

  /// Capturing from a device failed for another reason.
  deviceError,

  /// The signaling connection closed while the request was pending.
  connectionLost,

  /// Unexpected server or protocol error.
  internal;

  /// Maps a protocol `code` string to an [RtcErrorCode].
  static RtcErrorCode fromServer(String? code) => switch (code) {
    'unauthorized' => unauthorized,
    'token_expired' => tokenExpired,
    'forbidden' => forbidden,
    'room_not_found' => roomNotFound,
    'room_full' => roomFull,
    'room_ended' => roomEnded,
    'invalid_state' || 'not_joined' => invalidState,
    'limit_exceeded' => limitExceeded,
    'unavailable' => unavailable,
    _ => internal,
  };

  /// Whether reconnecting with the same token can never succeed.
  bool get isFatal => switch (this) {
    unauthorized || forbidden || roomNotFound || roomFull || roomEnded => true,
    _ => false,
  };
}

/// Error thrown by the SDK.
///
/// ```dart
/// try {
///   await session.connect(options);
/// } on RtcException catch (e) {
///   if (e.code == RtcErrorCode.roomFull) showRoomFullMessage();
/// }
/// ```
class RtcException implements Exception {
  /// Creates an exception.
  const RtcException(this.code, this.message);

  /// What went wrong. Branch on this, not on [message].
  final RtcErrorCode code;

  /// English description for logs. Never contains tokens.
  final String message;

  @override
  String toString() => 'RtcException(${code.name}): $message';
}
