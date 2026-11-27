import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart';

import 'connection_state.dart';
import 'errors.dart';
import 'events.dart';
import 'internal/backoff.dart';
import 'internal/interruptions.dart';
import 'internal/serial.dart';
import 'internal/signal_client.dart';
import 'internal/system_call.dart';
import 'internal/video_codec.dart';
import 'options.dart';
import 'participant.dart';
import 'subscription_policy.dart';

/// SDK version reported to the server for diagnostics.
const String sdkVersion = 'ecochain_rtc/0.1.0';

/// Simulcast layers published for the camera, lowest first. The bitrates
/// match the server's layer table (`server/src/sfu/layers.rs`).
const _cameraLayers = [('q', 4.0, 150000), ('h', 2.0, 500000), ('f', 1.0, 1500000)];

/// One meeting connection: signaling, media peers, local devices and the
/// participant list.
///
/// Keep the session outside of widgets (for example in an app level
/// controller) so that leaving the meeting screen does not end the call.
/// The UI only listens to it: the session is a [ChangeNotifier] and also
/// exposes one-off [events].
///
/// Lifecycle:
///
/// 1. [connect] joins the room. Calling it again while connecting or
///    connected returns the same future instead of joining twice.
/// 2. While connected, toggle devices with [setMicrophoneEnabled],
///    [setCameraEnabled], [startScreenShare]. Calls are queued, so rapid
///    taps never capture a device twice.
/// 3. Network loss moves to [RtcConnectionState.reconnecting]; the SDK
///    retries with backoff and jitter, resumes the server session and keeps
///    the microphone/camera choice.
/// 4. [leave] (or the host ending the meeting) releases camera, microphone,
///    peer connections and the socket. Results of operations that were
///    still running are discarded and cannot revive the session.
///
/// A session can be connected again after it ended. Call [dispose] when the
/// object is no longer needed.
///
/// ```dart
/// final session = RtcSession();
/// await session.connect(RtcConnectOptions(
///   url: Uri.parse('wss://meet.example.com/v1/rtc'),
///   token: token,
///   microphone: true,
/// ));
/// await session.setCameraEnabled(true);
/// // ...
/// await session.leave();
/// ```
///
/// Recording and file storage are not part of this stage. When they are
/// added, the server will record from the SFU (it already sees the media)
/// and encrypt each stored file with its own key; apps will download by
/// `fileId` after a server side permission check. No client API changes
/// are needed for audio/video calls.
class RtcSession extends ChangeNotifier {
  /// Creates an idle session.
  ///
  /// [interruptions] replaces the OS signals in tests.
  RtcSession({@visibleForTesting Stream<Set<RtcSendPauseReason>> Function()? interruptions})
    : _interruptions = interruptions ?? platformInterruptions;

  final Stream<Set<RtcSendPauseReason>> Function() _interruptions;
  StreamSubscription<Set<RtcSendPauseReason>>? _interruptionEvents;
  Set<RtcSendPauseReason> _pauseReasons = const {};
  Set<RtcSendPauseReason> _osPauseReasons = const {};
  bool _held = false;
  final _systemCall = SystemCall();

  final _events = StreamController<RtcEvent>.broadcast();
  final _media = Serial();
  final _eventQueue = Serial();

  RtcConnectionState _state = RtcConnectionState.idle;
  RtcDisconnectReason? _disconnectReason;
  RtcException? _lastError;
  bool _disposed = false;

  /// Incremented whenever a connection attempt starts or ends. Async work
  /// captures the value and drops its result if it changed meanwhile.
  int _generation = 0;
  Future<void>? _connectFuture;
  RtcConnectOptions? _options;

  SignalClient? _signal;
  String? _sessionId;
  String? _roomId;
  List<Map<String, dynamic>> _iceServers = const [];
  int _serverVideoLimit = 9;
  bool _reconnecting = false;

  RTCPeerConnection? _publisher;
  RTCDataChannel? _publisherChannel;
  final _senders = <RtcTrackSource, RTCRtpTransceiver>{};
  bool _publisherNeedsReset = false;
  RTCPeerConnection? _subscriber;
  final _midToTrack = <String, ({String trackId, String participantId})>{};

  final _localTracks = <RtcTrackSource, MediaStreamTrack>{};
  final _localStreams = <RtcTrackSource, MediaStream>{};
  bool _wantMicrophone = false;
  bool _wantCamera = false;
  bool _wantScreen = false;

  Map<String, dynamic>? _localInfo;
  final _remoteInfo = <String, Map<String, dynamic>>{};
  final _remoteMedia = <String, ({MediaStreamTrack track, MediaStream? stream})>{};
  List<String> _speakers = const [];

  List<String>? _visibleIds;
  String? _pinnedId;
  Map<String, RtcVideoQuality> _requestedVideos = {};
  Timer? _subscriptionTimer;
  Timer? _iceTimer;

  // ---------------------------------------------------------------------
  // Public state
  // ---------------------------------------------------------------------

  /// Current connection state.
  RtcConnectionState get connectionState => _state;

  /// Why the session ended; `null` unless [connectionState] is
  /// [RtcConnectionState.disconnected].
  RtcDisconnectReason? get disconnectReason => _disconnectReason;

  /// Last error that ended the session or failed a media operation.
  RtcException? get lastError => _lastError;

  /// Whether the session is connecting, connected or reconnecting. Use it
  /// for "return to meeting" buttons.
  bool get isActive =>
      _state == RtcConnectionState.connecting ||
      _state == RtcConnectionState.connected ||
      _state == RtcConnectionState.reconnecting;

  /// Room id from the token, once joined.
  String? get roomId => _roomId;

  /// Whether the microphone is on (the user's choice; kept across
  /// reconnects).
  bool get microphoneEnabled => _wantMicrophone && _localTracks.containsKey(RtcTrackSource.microphone);

  /// Whether the camera is on.
  bool get cameraEnabled => _wantCamera && _localTracks.containsKey(RtcTrackSource.camera);

  /// Whether this device shares its screen.
  bool get screenShareEnabled => _wantScreen && _localTracks.containsKey(RtcTrackSource.screen);

  /// Why nothing is sent right now; empty in normal operation.
  ///
  /// While the phone is locked or has a call, microphone, camera and screen
  /// are released and the others see them muted. Receiving goes on.
  /// Microphone and camera come back when the app is in front again, if the
  /// user still wants them on; a screen share has to be started again.
  Set<RtcSendPauseReason> get sendPauseReasons => _pauseReasons;

  /// Whether sending is blocked, see [sendPauseReasons].
  bool get sendingPaused => _pauseReasons.isNotEmpty;

  /// Whether the local participant is the host.
  bool get isHost => _localInfo?['role'] == 'host';

  /// Active speakers, loudest first.
  List<String> get activeSpeakerIds => _speakers;

  /// How many remote videos this device receives at most.
  int get maxVideoSubscriptions {
    final platformDefault = switch (defaultTargetPlatform) {
      TargetPlatform.android || TargetPlatform.iOS => 4,
      _ => 9,
    };
    final wanted = _options?.maxVideoSubscriptions ?? platformDefault;
    return wanted < _serverVideoLimit ? wanted : _serverVideoLimit;
  }

  /// The local participant, once joined.
  RtcParticipant? get localParticipant {
    final info = _localInfo;
    if (info == null) return null;
    final tracks = <RtcTrack>[
      for (final entry in _localTracks.entries)
        RtcTrack(
          id: 'local-${entry.key.wireName}',
          isLocal: true,
          source: entry.key,
          muted: false,
          simulcast: entry.key == RtcTrackSource.camera && (_options?.simulcast ?? false),
          mediaTrack: entry.value,
          renderStream: _localStreams[entry.key],
        ),
    ];
    return _participantFrom(info, isLocal: true, tracks: tracks);
  }

  /// Remote participants in join order. Each identity appears once.
  List<RtcParticipant> get remoteParticipants => [
    for (final info in _remoteInfo.values) _participantFrom(info, isLocal: false),
  ];

  /// Local participant first, then remote participants.
  List<RtcParticipant> get participants => [?localParticipant, ...remoteParticipants];

  /// One-off notifications. The stream stays open across reconnects and
  /// across later [connect] calls; it closes on [dispose].
  Stream<RtcEvent> get events => _events.stream;

  // ---------------------------------------------------------------------
  // Connect / leave
  // ---------------------------------------------------------------------

  /// Joins the room described by [options].
  ///
  /// Completes when the session is connected. Media flags in [options]
  /// are applied right after joining; a permission problem there does not
  /// fail the call but is reported in [lastError].
  ///
  /// Calling [connect] while a connection is in progress or established
  /// returns the pending future and ignores the new [options].
  ///
  /// Throws [RtcException] for invalid URLs ([RtcErrorCode.invalidUrl]),
  /// rejected tokens, a full or ended room, or when the server cannot be
  /// reached. The session is then [RtcConnectionState.disconnected].
  Future<void> connect(RtcConnectOptions options) {
    _checkNotDisposed();
    final pending = _connectFuture;
    if (pending != null && isActive) return pending;
    _validateUrl(options);
    final generation = ++_generation;
    _options = options;
    _wantMicrophone = options.microphone;
    _wantCamera = options.camera;
    _wantScreen = false;
    _disconnectReason = null;
    _lastError = null;
    _sessionId = null;
    unawaited(_interruptionEvents?.cancel());
    _pauseReasons = const {};
    _osPauseReasons = const {};
    _held = false;
    _interruptionEvents = _interruptions().listen((reasons) {
      _osPauseReasons = reasons;
      _updatePause();
    });
    _setState(RtcConnectionState.connecting);
    return _connectFuture = _connect(generation);
  }

  Future<void> _connect(int generation) async {
    try {
      await _join(generation);
    } on RtcException catch (e) {
      if (!_alive(generation)) return;
      await _end(_reasonFor(e.code), error: e);
      rethrow;
    }
    if (!_alive(generation)) return;
    _setState(RtcConnectionState.connected);
    if (defaultTargetPlatform == TargetPlatform.android || defaultTargetPlatform == TargetPlatform.iOS) {
      // Meetings are usually held hands-free.
      unawaited(Helper.setSpeakerphoneOn(true).catchError((_) {}));
    }
    await _applyLocalMedia(generation);
    // Last, so that it never competes with the device permission dialogs.
    if (_alive(generation)) _systemCall.requestPermission();
  }

  /// Leaves the meeting and releases camera, microphone, peer connections
  /// and the socket.
  ///
  /// Safe to call at any time and more than once; operations still running
  /// are cancelled and their results discarded.
  Future<void> leave() async {
    if (!isActive) {
      await _release();
      return;
    }
    final signal = _signal;
    _generation++;
    if (signal != null && !signal.isClosed) {
      try {
        await signal.request('leave').timeout(const Duration(seconds: 2));
      } catch (_) {
        // Leaving must work offline too; the server's grace period cleans up.
      }
    }
    await _end(RtcDisconnectReason.left);
  }

  /// Ends the meeting for everybody. Host only.
  ///
  /// Throws [RtcException] with [RtcErrorCode.forbidden] for non-hosts and
  /// [RtcErrorCode.notConnected] when not connected.
  Future<void> endMeeting() async {
    await _requireSignal().request('room.end');
  }

  /// Removes [participantId] from the meeting. Host only.
  Future<void> removeParticipant(String participantId) async {
    await _requireSignal().request('participant.remove', {'participant_id': participantId});
  }

  @override
  void dispose() {
    if (_disposed) return;
    _disposed = true;
    _generation++;
    _systemCall.end();
    unawaited(_release().whenComplete(_events.close));
    super.dispose();
  }

  /// Simulates a dropped signaling connection (no `leave`). The session
  /// reconnects and resumes as it would after a real network loss.
  @visibleForTesting
  Future<void> debugDropSignaling() async => _signal?.close();

  // ---------------------------------------------------------------------
  // Devices
  // ---------------------------------------------------------------------

  /// Asks the OS for microphone and/or camera permission by opening the
  /// devices briefly.
  ///
  /// Returns `true` when every requested device could be opened. On Android
  /// and iOS the app must declare the permissions (see the package README).
  static Future<bool> requestPermissions({bool microphone = true, bool camera = true}) async {
    if (!microphone && !camera) return true;
    try {
      final stream = await navigator.mediaDevices.getUserMedia({'audio': microphone, 'video': camera});
      for (final track in stream.getTracks()) {
        await track.stop();
      }
      await stream.dispose();
      return true;
    } catch (_) {
      return false;
    }
  }

  /// Turns the microphone on or off.
  ///
  /// Off stops the capture (the OS microphone indicator goes away). The
  /// choice is remembered: after a reconnect the microphone is restored.
  /// Calls are queued; rapid toggles end in the last requested state.
  ///
  /// Throws [RtcException] with [RtcErrorCode.permissionDenied] when the
  /// microphone cannot be opened, and [RtcErrorCode.notConnected] when the
  /// session is not active.
  Future<void> setMicrophoneEnabled(bool enabled) =>
      _setSource(RtcTrackSource.microphone, enabled, () => _wantMicrophone = enabled);

  /// Turns the camera on or off. Same rules as [setMicrophoneEnabled].
  Future<void> setCameraEnabled(bool enabled) =>
      _setSource(RtcTrackSource.camera, enabled, () => _wantCamera = enabled);

  /// Switches between front and back camera (mobile). No-op without camera.
  Future<void> switchCamera() => _media.run(() async {
    final track = _localTracks[RtcTrackSource.camera];
    if (track != null) await Helper.switchCamera(track);
  });

  /// Starts sharing the screen.
  ///
  /// On macOS and Windows the first screen is shared unless
  /// [desktopSourceId] names another `desktopCapturer` source. Android needs
  /// a media projection foreground service declared by the app; iOS needs a
  /// broadcast upload extension, which is not provided yet (see README).
  Future<void> startScreenShare({String? desktopSourceId}) {
    _screenSourceId = desktopSourceId;
    return _setSource(RtcTrackSource.screen, true, () => _wantScreen = true);
  }

  String? _screenSourceId;

  /// Stops sharing the screen.
  Future<void> stopScreenShare() => _setSource(RtcTrackSource.screen, false, () => _wantScreen = false);

  /// Tells the SDK which participants are on screen so it receives only the
  /// videos that matter (see [RtcVideoSubscriptionPolicy]).
  ///
  /// Call it whenever the visible tiles or the pinned participant change.
  /// Without a call, all remote participants count as visible in join order.
  void setVideoLayout({required List<String> visibleParticipantIds, String? pinnedParticipantId}) {
    _visibleIds = List.of(visibleParticipantIds);
    _pinnedId = pinnedParticipantId;
    _scheduleSubscriptions();
  }

  Future<void> _setSource(RtcTrackSource source, bool enabled, void Function() remember) {
    _checkNotDisposed();
    if (!isActive) {
      return Future.error(const RtcException(RtcErrorCode.notConnected, 'session is not active'));
    }
    remember();
    final generation = _generation;
    return _media.run(() async {
      if (!_alive(generation) || _state == RtcConnectionState.connecting) return;
      await _applySource(source, generation);
      _lastError = null;
      _notify();
    });
  }

  /// Applies all remembered device choices (after join or rejoin).
  Future<void> _applyLocalMedia(int generation) => _media.run(() async {
    _lastError = null;
    for (final source in RtcTrackSource.values) {
      if (!_alive(generation)) return;
      try {
        await _applySource(source, generation);
      } on RtcException catch (e) {
        // Joining succeeded; a device problem must not end the call, but it
        // must be visible (UI banner via lastError, and the log).
        debugPrint('ecochain_rtc: ${e.message}');
        _lastError = e;
      }
    }
    _notify();
  });

  void _updatePause() {
    final reasons = {..._osPauseReasons, if (_held) RtcSendPauseReason.phoneCall};
    if (!isActive || setEquals(reasons, _pauseReasons)) return;
    final changed = reasons.isEmpty == sendingPaused;
    _pauseReasons = reasons;
    if (changed) {
      if (sendingPaused) {
        _wantScreen = false;
        // Silence and blacken at once; releasing the devices is queued and
        // may have to wait for other media work.
        for (final track in _localTracks.values) {
          track.enabled = false;
        }
      }
      // While connecting, the devices are opened after joining anyway.
      if (_state != RtcConnectionState.connecting) unawaited(_applyLocalMedia(_generation));
    }
    _notify();
  }

  bool _wants(RtcTrackSource source) =>
      !sendingPaused &&
      switch (source) {
        RtcTrackSource.microphone => _wantMicrophone,
        RtcTrackSource.camera => _wantCamera,
        RtcTrackSource.screen => _wantScreen,
        RtcTrackSource.screenAudio => false,
      };

  /// Brings one source in line with the user's choice. Runs on [_media].
  Future<void> _applySource(RtcTrackSource source, int generation) async {
    final want = _wants(source);
    final current = _localTracks[source];
    if (want && current == null) {
      final (stream, track) = await _capture(source);
      if (!_alive(generation) || !_wants(source)) {
        // Left or toggled off while the OS was opening the device.
        await track.stop();
        await stream.dispose();
        return;
      }
      _localTracks[source] = track;
      _localStreams[source] = stream;
      if (source == RtcTrackSource.screen) {
        track.onEnded = () => unawaited(stopScreenShare().catchError((_) {}));
      }
      final sender = _senders[source];
      if (sender != null) {
        await sender.sender.replaceTrack(track);
        await _sendMute(source, false);
      } else {
        await _addSender(source, track, stream);
        await _negotiatePublisher(generation);
      }
    } else if (want && current != null) {
      // A pause may have ended before its release ran.
      if (!current.enabled) current.enabled = true;
    } else if (!want && current != null) {
      _localTracks.remove(source);
      final stream = _localStreams.remove(source);
      await _senders[source]?.sender.replaceTrack(null);
      await current.stop();
      await stream?.dispose();
      await _sendMute(source, true);
    }
  }

  /// Capture size to ask for. Phone sensors are 4:3 and a 16:9 mode cuts off
  /// part of the picture before it is even sent, so phones ask for 4:3.
  ///
  /// The plugin picks the mode with the smallest |dw| + |dh|. This target
  /// prefers 1280x960, then 1440x1080, and falls back to 1280x720 before any
  /// square (cropped) or full HD mode. Webcams are natively 16:9.
  static (int, int) get _cameraSize => switch (defaultTargetPlatform) {
    TargetPlatform.iOS || TargetPlatform.android when !kIsWeb => (1300, 990),
    _ => (1280, 720),
  };

  Future<(MediaStream, MediaStreamTrack)> _capture(RtcTrackSource source) async {
    try {
      final MediaStream stream;
      switch (source) {
        case RtcTrackSource.microphone:
          stream = await navigator.mediaDevices.getUserMedia({
            'audio': {'echoCancellation': true, 'noiseSuppression': true, 'autoGainControl': true},
            'video': false,
          });
        case RtcTrackSource.camera:
          stream = await navigator.mediaDevices.getUserMedia({
            'audio': false,
            'video': {
              'facingMode': 'user',
              // Plain numbers mean "ideal"; flutter_webrtc 1.6.3 ignores the
              // {'ideal': n} form on Android, iOS and macOS.
              'width': _cameraSize.$1,
              'height': _cameraSize.$2,
              'frameRate': 25,
            },
          });
        case RtcTrackSource.screen:
          stream = await _captureScreen();
        case RtcTrackSource.screenAudio:
          throw const RtcException(RtcErrorCode.deviceError, 'screen audio is not supported yet');
      }
      final tracks = source == RtcTrackSource.microphone ? stream.getAudioTracks() : stream.getVideoTracks();
      if (tracks.isEmpty) {
        await stream.dispose();
        throw const RtcException(RtcErrorCode.deviceError, 'device produced no track');
      }
      return (stream, tracks.first);
    } on RtcException {
      rethrow;
    } catch (e) {
      final text = e.toString().toLowerCase();
      final denied = text.contains('permission') || text.contains('notallowed') || text.contains('denied');
      throw RtcException(
        denied ? RtcErrorCode.permissionDenied : RtcErrorCode.deviceError,
        // Device errors carry no secrets; the text is what explains the
        // failure (busy device, missing hardware, denied permission).
        'cannot open ${source.name}: $e',
      );
    }
  }

  Future<MediaStream> _captureScreen() async {
    final desktop =
        !kIsWeb &&
        (defaultTargetPlatform == TargetPlatform.macOS ||
            defaultTargetPlatform == TargetPlatform.windows ||
            defaultTargetPlatform == TargetPlatform.linux);
    if (!desktop) {
      return navigator.mediaDevices.getDisplayMedia({'video': true, 'audio': false});
    }
    var sourceId = _screenSourceId;
    if (sourceId == null) {
      final sources = await desktopCapturer.getSources(types: [SourceType.Screen]);
      if (sources.isEmpty) throw const RtcException(RtcErrorCode.deviceError, 'no screen to share');
      sourceId = sources.first.id;
    }
    return navigator.mediaDevices.getDisplayMedia({
      'video': {
        'deviceId': {'exact': sourceId},
        'mandatory': {'frameRate': 15.0},
      },
      'audio': false,
    });
  }

  Future<void> _addSender(RtcTrackSource source, MediaStreamTrack track, MediaStream stream) async {
    final pc = _publisher;
    if (pc == null) return;
    final simulcast = source == RtcTrackSource.camera && (_options?.simulcast ?? true);
    final transceiver = await pc.addTransceiver(
      track: track,
      kind: source.isVideo ? RTCRtpMediaType.RTCRtpMediaTypeVideo : RTCRtpMediaType.RTCRtpMediaTypeAudio,
      init: RTCRtpTransceiverInit(
        direction: TransceiverDirection.SendOnly,
        streams: [stream],
        sendEncodings: switch (source) {
          RtcTrackSource.camera when simulcast => [
            for (final (rid, scale, bitrate) in _cameraLayers)
              RTCRtpEncoding(rid: rid, scaleResolutionDownBy: scale, maxBitrate: bitrate, active: true),
          ],
          RtcTrackSource.camera => [RTCRtpEncoding(maxBitrate: 800000, active: true)],
          RtcTrackSource.screen => [RTCRtpEncoding(maxBitrate: 1500000, maxFramerate: 15, active: true)],
          _ => null,
        },
      ),
    );
    _senders[source] = transceiver;
    if (source.isVideo) await configureVideoCodecs(transceiver);
  }

  Future<void> _sendMute(RtcTrackSource source, bool muted) async {
    final signal = _signal;
    if (signal == null || signal.isClosed || !_senders.containsKey(source)) return;
    try {
      await signal.request('track.mute', {'source': source.wireName, 'muted': muted});
    } on RtcException {
      // The next publisher offer carries the mute state anyway.
    }
  }

  // ---------------------------------------------------------------------
  // Join, resume, reconnect
  // ---------------------------------------------------------------------

  static const _maxHeartbeat = Duration(seconds: 2);
  static const _maxConnectTime = Duration(seconds: 4);

  /// Opens a socket and joins (or resumes) the room.
  Future<void> _join(int generation) async {
    final options = _options!;
    // An attempt started while offline must not hang long after the network
    // is back.
    final timeout = options.requestTimeout;
    final signal = await SignalClient.connect(
      options.url,
      timeout,
      connectTimeout: timeout < _maxConnectTime ? timeout : _maxConnectTime,
    );
    if (!_alive(generation)) {
      await signal.close();
      return;
    }
    // Events right after the join response (the first subscriber offer)
    // must not be lost, so listen before sending join and hold them until
    // the join is processed.
    final early = <SignalEvent>[];
    var joined = false;
    final subscription = signal.events.listen((event) {
      if (joined) {
        _queueEvent(generation, signal, event);
      } else {
        early.add(event);
      }
    });
    final Map<String, dynamic> data;
    try {
      data = await signal.request('join', {
        'token': options.token,
        'resume_session_id': ?_sessionId,
        'client': {'sdk': sdkVersion, 'platform': defaultTargetPlatform.name},
      });
    } catch (_) {
      await subscription.cancel();
      await signal.close();
      rethrow;
    }
    if (!_alive(generation)) {
      await subscription.cancel();
      await signal.close();
      return;
    }

    final old = _signal;
    _signal = signal;
    unawaited(old?.close());
    unawaited(signal.closed.then((code) => _onSignalClosed(generation, signal, code)));

    final resumed = data['resumed'] == true;
    _sessionId = data['session_id'] as String?;
    _localInfo = (data['participant'] as Map).cast<String, dynamic>();
    final room = (data['room'] as Map?)?.cast<String, dynamic>() ?? const {};
    _roomId = room['room_id'] as String?;
    _serverVideoLimit = (room['max_video_subscriptions'] as num?)?.toInt() ?? 9;
    _iceServers = [
      for (final server in (data['ice_servers'] as List? ?? const [])) (server as Map).cast<String, dynamic>(),
    ];
    _speakers = List<String>.from(data['speakers'] as List? ?? const []);

    // Replace the participant list wholesale: after a long outage people may
    // have left, and ids are stable so nothing is shown twice.
    final fresh = <String, Map<String, dynamic>>{
      for (final p in (data['participants'] as List? ?? const []))
        (p as Map)['participant_id'] as String: p.cast<String, dynamic>(),
    };
    for (final id in _remoteInfo.keys.where((id) => !fresh.containsKey(id)).toList()) {
      _dropRemoteTracksOf(id);
      _emit(RtcParticipantLeft(id, 'gone'));
    }
    _remoteInfo
      ..clear()
      ..addAll(fresh);

    if (!resumed) {
      // New server session: both peers start from scratch.
      await _closeSubscriber();
      _requestedVideos = {};
      _subscriptionFailures = 0;
      try {
        await _media.run(() => _rebuildPublisher(generation));
      } on RtcException {
        rethrow;
      } catch (e) {
        // Platform errors (missing plugin, native WebRTC failure) must not
        // escape as untyped exceptions or leave the socket open.
        await signal.close();
        throw RtcException(RtcErrorCode.internal, 'cannot create peer connection: ${e.runtimeType}');
      }
    }

    // The server's interval is the slowest allowed. A dead socket blocks ICE
    // restarts, so others see frozen video until it is noticed; resuming is
    // cheap and does not touch media that still flows.
    final requested = Duration(milliseconds: (data['heartbeat_interval_ms'] as num?)?.toInt() ?? 5000);
    final heartbeat = requested < _maxHeartbeat ? requested : _maxHeartbeat;
    signal.startHeartbeat(heartbeat, heartbeat * 2);

    joined = true;
    for (final event in early) {
      _queueEvent(generation, signal, event);
    }
    _scheduleSubscriptions();
    _notify();

    if (resumed) {
      // Media may have survived (signaling-only outage). If not, restart ICE.
      _scheduleIceCheck(generation, const Duration(seconds: 1));
    }
  }

  void _onSignalClosed(int generation, SignalClient signal, int? code) {
    if (!_alive(generation) || signal != _signal) return;
    switch (code) {
      case CloseCodes.sessionReplaced:
        unawaited(_end(RtcDisconnectReason.replacedByOtherSession));
      case CloseCodes.unauthorized:
        unawaited(_end(RtcDisconnectReason.unauthorized));
      case CloseCodes.roomEnded:
        unawaited(_end(RtcDisconnectReason.roomEnded));
      case CloseCodes.removed:
        unawaited(_end(RtcDisconnectReason.removed));
      case CloseCodes.normal:
        if (_state != RtcConnectionState.disconnected) unawaited(_reconnect(generation));
      default:
        unawaited(_reconnect(generation));
    }
  }

  /// Retries joining with exponential backoff and jitter until it works, a
  /// fatal error arrives, the policy gives up or the user leaves.
  Future<void> _reconnect(int generation) async {
    if (_reconnecting || !_alive(generation)) return;
    _reconnecting = true;
    _setState(RtcConnectionState.reconnecting);
    final policy = _options!.reconnect;
    final backoff = Backoff(policy);
    final started = DateTime.now();
    try {
      while (_alive(generation)) {
        try {
          await _join(generation);
          if (!_alive(generation)) return;
          _setState(RtcConnectionState.connected);
          await _applyLocalMedia(generation);
          return;
        } on RtcException catch (e) {
          if (!_alive(generation)) return;
          if (e.code == RtcErrorCode.tokenExpired) {
            final provider = _options!.tokenProvider;
            if (provider == null) {
              await _end(RtcDisconnectReason.unauthorized, error: e);
              return;
            }
            try {
              _options = _options!.withToken(await provider());
            } catch (_) {
              // Treated like a network failure: retried with backoff.
            }
          } else if (e.code.isFatal) {
            await _end(_reasonFor(e.code), error: e);
            return;
          }
        }
        if (DateTime.now().difference(started) > policy.giveUpAfter) {
          await _end(
            RtcDisconnectReason.networkFailed,
            error: const RtcException(RtcErrorCode.connectionLost, 'could not reconnect'),
          );
          return;
        }
        await Future<void>.delayed(backoff.next());
      }
    } finally {
      _reconnecting = false;
    }
  }

  // ---------------------------------------------------------------------
  // Server events
  // ---------------------------------------------------------------------

  void _queueEvent(int generation, SignalClient signal, SignalEvent event) {
    unawaited(
      _eventQueue.run(() async {
        if (!_alive(generation) || signal != _signal) return;
        try {
          await _handleEvent(generation, event);
        } catch (e) {
          debugPrint('ecochain_rtc: failed to handle ${event.type}: $e');
        }
      }),
    );
  }

  Future<void> _handleEvent(int generation, SignalEvent event) async {
    final data = event.data;
    switch (event.type) {
      case 'participant.joined':
      case 'participant.updated':
        final info = (data['participant'] as Map).cast<String, dynamic>();
        final id = info['participant_id'] as String;
        final known = _remoteInfo.containsKey(id);
        _remoteInfo[id] = info;
        _dropMissingRemoteTracks(id);
        final participant = _participantFrom(info, isLocal: false);
        _emit(known ? RtcParticipantUpdated(participant) : RtcParticipantJoined(participant));
        _scheduleSubscriptions();
      case 'participant.left':
        final id = data['participant_id'] as String;
        if (_remoteInfo.remove(id) != null) {
          _dropRemoteTracksOf(id);
          _emit(RtcParticipantLeft(id, data['reason'] as String? ?? 'left'));
        }
        _scheduleSubscriptions();
      case 'speakers.changed':
        _speakers = [for (final s in (data['speakers'] as List? ?? const [])) (s as Map)['participant_id'] as String];
        _emit(RtcActiveSpeakersChanged(_speakers));
        _scheduleSubscriptions();
      case 'subscriber.offer':
        await _onSubscriberOffer(generation, data);
      case 'peer.failed':
        if (data['target'] == 'subscriber') {
          await _closeSubscriber();
          await _signal?.request('subscriber.reset');
        } else {
          await _media.run(() => _rebuildPublisher(generation));
        }
      case 'room.ended':
        await _end(RtcDisconnectReason.roomEnded);
      case 'removed':
        await _end(RtcDisconnectReason.removed);
      case 'session.replaced':
        await _end(RtcDisconnectReason.replacedByOtherSession);
    }
    _notify();
  }

  // ---------------------------------------------------------------------
  // Publisher peer
  // ---------------------------------------------------------------------

  Map<String, dynamic> get _peerConfiguration => {
    'iceServers': _iceServers,
    'sdpSemantics': 'unified-plan',
    'bundlePolicy': 'max-bundle',
    'rtcpMuxPolicy': 'require',
  };

  /// Creates a fresh publisher peer with the current local tracks and
  /// sends a `reset` offer. Runs on [_media].
  Future<void> _rebuildPublisher(int generation) async {
    await _closePublisher();
    final pc = await createPeerConnection(_peerConfiguration);
    if (!_alive(generation)) {
      await pc.close();
      await pc.dispose();
      return;
    }
    _publisher = pc;
    pc.onIceConnectionState = (state) => _onIceState(generation, pc, state);
    // The data channel gives the peer an m-line even before any track is
    // published, so ICE and DTLS are ready when the user unmutes.
    _publisherChannel = await pc.createDataChannel('ecochain-pub', RTCDataChannelInit());
    for (final source in RtcTrackSource.values) {
      final track = _localTracks[source];
      final stream = _localStreams[source];
      if (track != null && stream != null) await _addSender(source, track, stream);
    }
    _publisherNeedsReset = true;
    await _negotiatePublisher(generation);
  }

  /// Offer/answer on the publisher. Runs on [_media].
  Future<void> _negotiatePublisher(int generation, {bool iceRestart = false}) async {
    final pc = _publisher;
    final signal = _signal;
    if (pc == null || signal == null || !_alive(generation)) return;
    final offer = await pc.createOffer(iceRestart ? {'iceRestart': true} : {});
    await pc.setLocalDescription(offer);
    final transceivers = await pc.getTransceivers();
    final tracks = <Map<String, dynamic>>[];
    for (final entry in _senders.entries) {
      final senderId = entry.value.sender.senderId;
      final mid = transceivers.where((t) => t.sender.senderId == senderId).map((t) => t.mid).firstOrNull;
      if (mid == null || mid.isEmpty) continue;
      final simulcast = entry.key == RtcTrackSource.camera && (_options?.simulcast ?? true);
      tracks.add({
        'mid': mid,
        'source': entry.key.wireName,
        'muted': !_localTracks.containsKey(entry.key),
        'layers': simulcast ? [for (final (rid, _, _) in _cameraLayers) rid] : <String>[],
      });
    }
    final reset = _publisherNeedsReset;
    _publisherNeedsReset = false;
    final answer = await signal.request('publisher.offer', {'sdp': offer.sdp, 'tracks': tracks, 'reset': reset});
    if (!_alive(generation) || pc != _publisher) return;
    await pc.setRemoteDescription(RTCSessionDescription(answer['sdp'] as String, 'answer'));
  }

  Future<void> _closePublisher() async {
    final pc = _publisher;
    _publisher = null;
    _senders.clear();
    final channel = _publisherChannel;
    _publisherChannel = null;
    await channel?.close().catchError((_) {});
    if (pc != null) {
      await pc.close().catchError((_) {});
      await pc.dispose().catchError((_) {});
    }
  }

  // ---------------------------------------------------------------------
  // Subscriber peer
  // ---------------------------------------------------------------------

  Future<void> _onSubscriberOffer(int generation, Map<String, dynamic> data) async {
    var pc = _subscriber;
    if (pc == null) {
      pc = await createPeerConnection(_peerConfiguration);
      if (!_alive(generation)) {
        await pc.close();
        await pc.dispose();
        return;
      }
      _subscriber = pc;
      final created = pc;
      pc.onTrack = (event) => _onRemoteTrack(generation, created, event);
      pc.onIceConnectionState = (state) => _onIceState(generation, created, state);
    }
    _midToTrack
      ..clear()
      ..addEntries([
        for (final t in (data['tracks'] as List? ?? const []))
          MapEntry((t as Map)['mid'] as String, (
            trackId: t['track_id'] as String,
            participantId: t['participant_id'] as String,
          )),
      ]);
    final RTCSessionDescription answer;
    try {
      await pc.setRemoteDescription(RTCSessionDescription(data['sdp'] as String, 'offer'));
      answer = await pc.createAnswer();
      await pc.setLocalDescription(answer);
    } catch (e) {
      // The peer and the server disagree about the negotiated state (for
      // example after a lost answer). Patching is not possible; start the
      // subscriber over so remote media comes back by itself.
      debugPrint('ecochain_rtc: subscriber offer rejected, resetting: $e');
      if (!_alive(generation) || pc != _subscriber) return;
      await _closeSubscriber();
      await _signal?.request('subscriber.reset');
      return;
    }
    if (!_alive(generation) || pc != _subscriber) return;
    try {
      await _signal?.request('subscriber.answer', {'sdp': answer.sdp, 'revision': data['revision']});
    } on RtcException catch (e) {
      // A newer offer is already queued when the revision is outdated.
      if (e.code != RtcErrorCode.invalidState) rethrow;
    }
    // Forget media of tracks that are no longer forwarded.
    final forwarded = {for (final t in _midToTrack.values) t.trackId};
    for (final trackId in _remoteMedia.keys.where((id) => !forwarded.contains(id)).toList()) {
      _disposeRemoteMedia(trackId);
    }
  }

  Future<void> _onRemoteTrack(int generation, RTCPeerConnection pc, RTCTrackEvent event) async {
    if (!_alive(generation) || pc != _subscriber) return;
    final mid = event.transceiver?.mid;
    final mapping = mid == null ? null : _midToTrack[mid];
    if (mapping == null) return;
    MediaStream? stream;
    if (event.track.kind == 'video') {
      stream = await createLocalMediaStream('r-${mapping.trackId}');
      await stream.addTrack(event.track);
      if (!_alive(generation)) {
        await stream.dispose();
        return;
      }
    }
    _disposeRemoteMedia(mapping.trackId);
    _remoteMedia[mapping.trackId] = (track: event.track, stream: stream);
    final participant = _remoteInfo[mapping.participantId];
    if (participant != null) {
      final snapshot = _participantFrom(participant, isLocal: false);
      final track = snapshot.tracks.where((t) => t.id == mapping.trackId).firstOrNull;
      if (track != null) _emit(RtcTrackSubscribed(mapping.participantId, track));
    }
    _notify();
  }

  Future<void> _closeSubscriber() async {
    final pc = _subscriber;
    _subscriber = null;
    _midToTrack.clear();
    for (final id in _remoteMedia.keys.toList()) {
      _disposeRemoteMedia(id);
    }
    if (pc != null) {
      await pc.close().catchError((_) {});
      await pc.dispose().catchError((_) {});
    }
  }

  void _disposeRemoteMedia(String trackId) {
    final media = _remoteMedia.remove(trackId);
    unawaited(media?.stream?.dispose().catchError((_) {}));
  }

  void _dropRemoteTracksOf(String participantId) {
    for (final entry in _midToTrack.entries.where((e) => e.value.participantId == participantId).toList()) {
      _disposeRemoteMedia(entry.value.trackId);
    }
  }

  void _dropMissingRemoteTracks(String participantId) {
    final info = _remoteInfo[participantId];
    final published = {for (final t in (info?['tracks'] as List? ?? const [])) (t as Map)['track_id']};
    for (final entry in _midToTrack.entries.where((e) => e.value.participantId == participantId).toList()) {
      if (!published.contains(entry.value.trackId)) _disposeRemoteMedia(entry.value.trackId);
    }
  }

  // ---------------------------------------------------------------------
  // ICE monitoring
  // ---------------------------------------------------------------------

  void _onIceState(int generation, RTCPeerConnection pc, RTCIceConnectionState state) {
    if (!_alive(generation)) return;
    if (state == RTCIceConnectionState.RTCIceConnectionStateFailed ||
        state == RTCIceConnectionState.RTCIceConnectionStateDisconnected) {
      // "disconnected" often heals by itself within a second or two
      // (short radio stalls); give it a moment before restarting ICE.
      // "failed" never heals without a restart.
      final failed = state == RTCIceConnectionState.RTCIceConnectionStateFailed;
      _scheduleIceCheck(generation, failed ? Duration.zero : const Duration(seconds: 2));
    }
  }

  void _scheduleIceCheck(int generation, Duration delay, {int attempt = 0}) {
    _iceTimer?.cancel();
    _iceTimer = Timer(delay, () => unawaited(_checkIce(generation, attempt)));
  }

  /// Restarts ICE on peers that are not connected. After two failed
  /// restarts both peers are rebuilt (full media reconnect).
  Future<void> _checkIce(int generation, int attempt) async {
    if (!_alive(generation) || _state != RtcConnectionState.connected) return;
    bool healthy(RTCPeerConnection? pc) {
      final state = pc?.iceConnectionState;
      return pc == null ||
          state == RTCIceConnectionState.RTCIceConnectionStateConnected ||
          state == RTCIceConnectionState.RTCIceConnectionStateCompleted;
    }

    final publisherOk = healthy(_publisher);
    final subscriberOk = healthy(_subscriber);
    if (publisherOk && subscriberOk) return;
    try {
      if (attempt >= 2) {
        await _closeSubscriber();
        await _signal?.request('subscriber.reset');
        await _media.run(() => _rebuildPublisher(generation));
      } else {
        if (!publisherOk) await _media.run(() => _negotiatePublisher(generation, iceRestart: true));
        if (!subscriberOk) await _signal?.request('ice.restart', {'target': 'subscriber'});
      }
    } on RtcException {
      // Signaling trouble is handled by the reconnect loop.
    }
    _scheduleIceCheck(generation, const Duration(seconds: 5), attempt: attempt + 1);
  }

  // ---------------------------------------------------------------------
  // Video subscriptions
  // ---------------------------------------------------------------------

  static const _maxSubscriptionRetries = 5;
  int _subscriptionFailures = 0;

  // Debounced: scrolling a gallery or a burst of join events should cause
  // one update, not dozens.
  void _scheduleSubscriptions([Duration delay = const Duration(milliseconds: 150)]) {
    _subscriptionTimer?.cancel();
    _subscriptionTimer = Timer(delay, () => unawaited(_updateSubscriptions(_generation)));
  }

  Future<void> _updateSubscriptions(int generation) async {
    final signal = _signal;
    if (!_alive(generation) || signal == null || _state != RtcConnectionState.connected) return;
    final remote = remoteParticipants;
    final requests = RtcVideoSubscriptionPolicy.select(
      remote: remote,
      visibleIds: _visibleIds ?? [for (final p in remote) p.id],
      pinnedId: _pinnedId,
      speakerIds: _speakers,
      maxVideos: maxVideoSubscriptions,
    );
    final wanted = {for (final r in requests) r.trackId: r.quality};
    // The server forgets subscriptions of unpublished tracks by itself, and
    // rejects a whole update that names a track it no longer knows.
    final published = {
      for (final p in remote)
        for (final t in p.tracks) t.id,
    };
    final changes = <Map<String, dynamic>>[
      for (final id in _requestedVideos.keys)
        if (!wanted.containsKey(id) && published.contains(id)) {'track_id': id, 'subscribed': false},
      for (final entry in wanted.entries)
        if (_requestedVideos[entry.key] != entry.value)
          {'track_id': entry.key, 'subscribed': true, 'quality': entry.value.name},
    ];
    if (changes.isEmpty) {
      _requestedVideos = wanted;
      return;
    }
    final previous = _requestedVideos;
    _requestedVideos = wanted;
    try {
      await signal.request('subscription.update', {'changes': changes});
      _subscriptionFailures = 0;
    } on RtcException {
      if (!_alive(generation)) return;
      _requestedVideos = previous;
      // For example a track vanished while the request was on its way. The
      // event that told us may already have been handled, so do not wait
      // for another one. Bounded: a permanent rejection must not loop.
      if (++_subscriptionFailures <= _maxSubscriptionRetries) {
        _scheduleSubscriptions(const Duration(seconds: 1));
      }
    }
  }

  // ---------------------------------------------------------------------
  // Statistics
  // ---------------------------------------------------------------------

  /// Totals of media received on this device since the subscriber peer was
  /// created. Useful for network indicators and for verifying that media
  /// actually flows.
  ///
  /// Returns zeros when not connected.
  Future<RtcReceiveStats> getReceiveStats() async {
    final pc = _subscriber;
    if (pc == null) return const RtcReceiveStats(audioBytes: 0, videoBytes: 0, videoFramesDecoded: 0, packetsLost: 0);
    var audio = 0, video = 0, frames = 0, lost = 0;
    for (final report in await pc.getStats()) {
      if (report.type != 'inbound-rtp') continue;
      final values = report.values;
      final bytes = (values['bytesReceived'] as num?)?.toInt() ?? 0;
      lost += (values['packetsLost'] as num?)?.toInt() ?? 0;
      if (values['kind'] == 'audio' || values['mediaType'] == 'audio') {
        audio += bytes;
      } else {
        video += bytes;
        frames += (values['framesDecoded'] as num?)?.toInt() ?? 0;
      }
    }
    return RtcReceiveStats(audioBytes: audio, videoBytes: video, videoFramesDecoded: frames, packetsLost: lost);
  }

  // ---------------------------------------------------------------------
  // Helpers
  // ---------------------------------------------------------------------

  RtcParticipant _participantFrom(Map<String, dynamic> info, {required bool isLocal, List<RtcTrack>? tracks}) {
    final id = info['participant_id'] as String;
    return RtcParticipant(
      id: id,
      identity: info['identity'] as String? ?? '',
      name: info['name'] as String? ?? '',
      role: info['role'] == 'host' ? RtcRole.host : RtcRole.participant,
      isLocal: isLocal,
      connected: isLocal ? _state == RtcConnectionState.connected : info['connected'] != false,
      isSpeaking: _speakers.contains(id),
      tracks:
          tracks ??
          [for (final t in (info['tracks'] as List? ?? const [])) _remoteTrack((t as Map).cast<String, dynamic>())],
    );
  }

  RtcTrack _remoteTrack(Map<String, dynamic> t) {
    final id = t['track_id'] as String;
    final media = _remoteMedia[id];
    return RtcTrack(
      id: id,
      source: RtcTrackSource.parse(t['source'] as String?, video: t['kind'] == 'video'),
      muted: t['muted'] == true,
      simulcast: t['simulcast'] == true,
      mediaTrack: media?.track,
      renderStream: media?.stream,
      readStats: kDebugMode && media != null && t['kind'] == 'video'
          ? () => _getRemoteTrackStats(id, media.track)
          : null,
    );
  }

  Future<List<StatsReport>> _getRemoteTrackStats(String id, MediaStreamTrack track) async {
    final pc = _subscriber;
    if (pc == null || _remoteMedia[id]?.track != track) return const [];
    final reports = await pc.getStats(track);
    if (pc != _subscriber || _remoteMedia[id]?.track != track) return const [];
    return reports;
  }

  SignalClient _requireSignal() {
    final signal = _signal;
    if (_state != RtcConnectionState.connected || signal == null) {
      throw const RtcException(RtcErrorCode.notConnected, 'session is not connected');
    }
    return signal;
  }

  /// Ends the session and releases everything.
  Future<void> _end(RtcDisconnectReason reason, {RtcException? error}) async {
    if (_state == RtcConnectionState.disconnected) return;
    _generation++;
    _disconnectReason = reason;
    if (error != null) _lastError = error;
    await _release();
    _setState(RtcConnectionState.disconnected);
    _emit(RtcSessionEnded(reason));
  }

  /// Stops devices, closes peers and the socket, clears participant state.
  Future<void> _release() async {
    unawaited(_interruptionEvents?.cancel());
    _interruptionEvents = null;
    _pauseReasons = const {};
    _osPauseReasons = const {};
    _held = false;
    _subscriptionTimer?.cancel();
    _iceTimer?.cancel();
    final signal = _signal;
    _signal = null;
    await signal?.close();
    // Wait for queued media work to notice the new generation first, so no
    // capture finishes after this point.
    await _media.run(() async {});
    await _closePublisher();
    await _closeSubscriber();
    for (final track in _localTracks.values) {
      await track.stop().catchError((_) {});
    }
    for (final stream in _localStreams.values) {
      await stream.dispose().catchError((_) {});
    }
    _localTracks.clear();
    _localStreams.clear();
    _remoteInfo.clear();
    _localInfo = null;
    _speakers = const [];
    _requestedVideos = {};
    _connectFuture = null;
    _notify();
  }

  bool _alive(int generation) => !_disposed && generation == _generation;

  RtcDisconnectReason _reasonFor(RtcErrorCode code) => switch (code) {
    RtcErrorCode.unauthorized ||
    RtcErrorCode.tokenExpired ||
    RtcErrorCode.forbidden => RtcDisconnectReason.unauthorized,
    RtcErrorCode.roomFull => RtcDisconnectReason.roomFull,
    RtcErrorCode.roomEnded => RtcDisconnectReason.roomEnded,
    RtcErrorCode.roomNotFound => RtcDisconnectReason.roomNotFound,
    RtcErrorCode.connectionLost || RtcErrorCode.timeout => RtcDisconnectReason.networkFailed,
    _ => RtcDisconnectReason.error,
  };

  void _validateUrl(RtcConnectOptions options) {
    final url = options.url;
    if (url.scheme == 'wss' && url.host.isNotEmpty) return;
    if (url.scheme == 'ws' && options.allowInsecureLocalhost && _isLocalHost(url.host)) return;
    throw RtcException(
      RtcErrorCode.invalidUrl,
      url.scheme == 'ws'
          ? 'plain ws:// is only allowed for local development hosts with allowInsecureLocalhost'
          : 'url must be wss://host/v1/rtc',
    );
  }

  static bool _isLocalHost(String host) {
    if (host == 'localhost' || host.endsWith('.local') || host == '::1' || host == '[::1]') return true;
    final parts = host.split('.').map(int.tryParse).toList();
    if (parts.length != 4 || parts.any((p) => p == null)) return false;
    final [a, b, ..._] = parts.cast<int>();
    return a == 127 || a == 10 || (a == 192 && b == 168) || (a == 172 && b >= 16 && b <= 31);
  }

  void _setState(RtcConnectionState state) {
    if (_state == state) return;
    _state = state;
    _emit(RtcConnectionStateChanged(state));
    _notify();
  }

  void _emit(RtcEvent event) {
    if (!_events.isClosed) _events.add(event);
  }

  void _notify() {
    if (_disposed) return;
    _syncSystemCall();
    notifyListeners();
  }

  void _syncSystemCall() {
    final call = _options?.systemCall;
    if (call == null || !isActive) {
      _systemCall.end();
      return;
    }
    _systemCall.show(
      call,
      connected: _state != RtcConnectionState.connecting,
      muted: !_wantMicrophone || sendingPaused,
      paused: sendingPaused,
      actions: (
        onMute: (muted) {
          // The lock screen must not undo a pause, not even for later.
          if (sendingPaused || muted == !_wantMicrophone) {
            unawaited(Future.microtask(_notify));
            return;
          }
          unawaited(
            setMicrophoneEnabled(!muted).catchError((Object e) {
              if (e is RtcException) _lastError = e;
              _notify();
            }),
          );
        },
        onHold: (held) {
          _held = held;
          _updatePause();
        },
        onHangUp: () => unawaited(leave()),
      ),
    );
  }

  void _checkNotDisposed() {
    if (_disposed) throw StateError('RtcSession was disposed');
  }
}

/// Received media totals, see [RtcSession.getReceiveStats].
@immutable
class RtcReceiveStats {
  /// Creates stats.
  const RtcReceiveStats({
    required this.audioBytes,
    required this.videoBytes,
    required this.videoFramesDecoded,
    required this.packetsLost,
  });

  /// Audio payload bytes received.
  final int audioBytes;

  /// Video payload bytes received.
  final int videoBytes;

  /// Video frames decoded.
  final int videoFramesDecoded;

  /// RTP packets reported lost (before retransmission).
  final int packetsLost;

  @override
  String toString() =>
      'RtcReceiveStats(audio: $audioBytes B, video: $videoBytes B, frames: $videoFramesDecoded, lost: $packetsLost)';
}
