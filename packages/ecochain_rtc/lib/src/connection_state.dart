/// Lifecycle of an [RtcSession].
///
/// ```text
/// idle ──connect()──▶ connecting ──▶ connected ◀──▶ reconnecting
///                         │              │              │
///                         └──────────────┴──────────────┴──▶ disconnected
/// ```
///
/// `disconnected` is final for this connection attempt; [RtcSession.disconnectReason]
/// says why. Calling `connect` again starts a new attempt.
enum RtcConnectionState {
  /// No connection was attempted yet.
  idle,

  /// Opening the signaling socket and joining the room.
  connecting,

  /// Joined; media flows.
  connected,

  /// The network dropped. The SDK retries with backoff and keeps the
  /// participant's seat on the server (resume). Local camera and microphone
  /// choices are kept and re-applied.
  reconnecting,

  /// The session ended. See [RtcDisconnectReason].
  disconnected,
}

/// Why a session reached [RtcConnectionState.disconnected].
enum RtcDisconnectReason {
  /// [RtcSession.leave] was called.
  left,

  /// The host ended the meeting for everybody.
  roomEnded,

  /// The host removed this participant.
  removed,

  /// The same identity joined from another device or app instance.
  replacedByOtherSession,

  /// The token was rejected or expired and no new token was available.
  unauthorized,

  /// The room is full.
  roomFull,

  /// The room does not exist.
  roomNotFound,

  /// Reconnecting failed for longer than
  /// [RtcReconnectPolicy.giveUpAfter].
  networkFailed,

  /// Any other server or protocol error.
  error,
}

/// Why the SDK temporarily sends neither audio nor video.
enum RtcSendPauseReason {
  /// The screen is locked.
  screenLocked,

  /// A phone call rings or is in progress.
  phoneCall,
}
