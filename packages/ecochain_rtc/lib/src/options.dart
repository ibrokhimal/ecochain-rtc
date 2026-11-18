import 'package:flutter/foundation.dart';

/// Retry policy for lost connections.
///
/// The delay before attempt `n` (starting at 0) is
/// `min(maxDelay, initialDelay * 2^n)`, multiplied by a random factor in
/// `[0.5, 1.0)` ("equal jitter"). Jitter spreads reconnects of many clients
/// after a server restart or a cell tower handover.
///
/// Retrying stops when:
/// * the session reconnects,
/// * the server answers with a fatal code (unauthorized, forbidden, room
///   full, room ended, removed, replaced),
/// * [giveUpAfter] passed since the connection was lost, or
/// * [RtcSession.leave] is called.
@immutable
class RtcReconnectPolicy {
  /// Creates a policy.
  const RtcReconnectPolicy({
    this.initialDelay = const Duration(milliseconds: 500),
    this.maxDelay = const Duration(seconds: 8),
    this.giveUpAfter = const Duration(seconds: 60),
  });

  /// Delay before the first retry.
  final Duration initialDelay;

  /// Upper bound of a single delay.
  final Duration maxDelay;

  /// Total time after which the session is ended with
  /// [RtcDisconnectReason.networkFailed].
  final Duration giveUpAfter;
}

/// Everything needed to join a room.
///
/// ```dart
/// final options = RtcConnectOptions(
///   url: Uri.parse('wss://meet.example.com/v1/rtc'),
///   token: await backend.fetchMeetingToken(roomId),
///   tokenProvider: () => backend.fetchMeetingToken(roomId),
/// );
/// ```
@immutable
class RtcConnectOptions {
  /// Creates options.
  const RtcConnectOptions({
    required this.url,
    required this.token,
    this.tokenProvider,
    this.microphone = false,
    this.camera = false,
    this.simulcast = true,
    this.maxVideoSubscriptions,
    this.reconnect = const RtcReconnectPolicy(),
    this.allowInsecureLocalhost = false,
    this.requestTimeout = const Duration(seconds: 10),
  });

  /// Signaling endpoint, for example `wss://meet.example.com/v1/rtc`.
  final Uri url;

  /// Room token issued by your backend. It binds this connection to one room
  /// and one participant identity.
  final String token;

  /// Called when the server reports `token_expired` during a reconnect.
  /// Return a fresh token for the same room and identity. When `null`, an
  /// expired token ends the session with [RtcDisconnectReason.unauthorized].
  final Future<String> Function()? tokenProvider;

  /// Start with the microphone on. Requires microphone permission.
  final bool microphone;

  /// Start with the camera on. Requires camera permission.
  final bool camera;

  /// Publish the camera in three simulcast layers so receivers can pick a
  /// size that fits their tile and bandwidth.
  final bool simulcast;

  /// How many remote videos this device decodes at once. Defaults to 4 on
  /// Android/iOS and 9 on desktop, capped by the server limit.
  final int? maxVideoSubscriptions;

  /// Reconnect behavior.
  final RtcReconnectPolicy reconnect;

  /// Allow plain `ws://` to loopback and private LAN addresses. For local
  /// development only; production must use `wss://` with a valid
  /// certificate. Certificate checks are never disabled by the SDK.
  final bool allowInsecureLocalhost;

  /// Timeout of a single signaling request.
  final Duration requestTimeout;

  /// Returns a copy with another token (used after a token refresh).
  RtcConnectOptions withToken(String token) => RtcConnectOptions(
    url: url,
    token: token,
    tokenProvider: tokenProvider,
    microphone: microphone,
    camera: camera,
    simulcast: simulcast,
    maxVideoSubscriptions: maxVideoSubscriptions,
    reconnect: reconnect,
    allowInsecureLocalhost: allowInsecureLocalhost,
    requestTimeout: requestTimeout,
  );
}
