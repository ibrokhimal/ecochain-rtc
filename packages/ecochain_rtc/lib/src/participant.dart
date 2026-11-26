import 'package:flutter/foundation.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart';

/// What a track carries.
enum RtcTrackSource {
  /// Microphone audio.
  microphone,

  /// Camera video.
  camera,

  /// Screen share video.
  screen,

  /// Audio captured with a screen share.
  screenAudio;

  /// Protocol name, for example `screen_audio`.
  String get wireName => this == screenAudio ? 'screen_audio' : name;

  /// Parses a protocol name. Unknown values map to [camera] for video and
  /// [microphone] for audio so a newer server cannot break older apps.
  static RtcTrackSource parse(String? value, {required bool video}) => switch (value) {
    'microphone' => microphone,
    'camera' => camera,
    'screen' => screen,
    'screen_audio' => screenAudio,
    _ => video ? camera : microphone,
  };

  /// Whether the source is a video source.
  bool get isVideo => this == camera || this == screen;
}

/// Participant role.
enum RtcRole {
  /// May end the meeting and remove participants.
  host,

  /// Regular participant.
  participant,
}

/// A published audio or video track.
///
/// Remote tracks get a [mediaTrack] once this device is subscribed and the
/// media arrived; render video with [RtcVideoView].
class RtcTrack {
  /// Creates a track description. Apps do not create tracks themselves.
  RtcTrack({
    required this.id,
    required this.source,
    required this.muted,
    required this.simulcast,
    this.isLocal = false,
    this.readStats,
    this.mediaTrack,
    this.renderStream,
  });

  /// Room-unique id assigned by the server (`TR_...`), or `local-<source>`
  /// for a local track before the server knows it.
  final String id;

  /// What the track carries.
  final RtcTrackSource source;

  /// Whether the publisher muted it. Muted video should show an avatar.
  final bool muted;

  /// Whether the publisher sends simulcast layers.
  final bool simulcast;

  /// Whether this track is a local preview rather than received video.
  final bool isLocal;

  /// WebRTC stats selected for this track, available for debug diagnostics.
  /// The view polls this only in debug builds and while its tile is mounted.
  final Future<List<StatsReport>> Function()? readStats;

  /// The underlying WebRTC track, once media is available.
  final MediaStreamTrack? mediaTrack;

  /// A stream that contains only [mediaTrack]; renderers need a stream.
  final MediaStream? renderStream;

  /// Whether this is video.
  bool get isVideo => source.isVideo;

  /// Whether video can be shown right now.
  bool get canRender => isVideo && !muted && renderStream != null;
}

/// A participant in the room, local or remote.
///
/// Instances are immutable snapshots; listen to the session for changes.
/// The [id] is stable for the identity while it stays in the room, even
/// across reconnects, so it is safe to use as a widget key.
@immutable
class RtcParticipant {
  /// Creates a snapshot. Apps do not create participants themselves.
  const RtcParticipant({
    required this.id,
    required this.identity,
    required this.name,
    required this.role,
    required this.isLocal,
    required this.connected,
    required this.tracks,
    this.isSpeaking = false,
  });

  /// Server assigned id (`PA_...`).
  final String id;

  /// Identity from the token (for example a Messenger user id).
  final String identity;

  /// Display name.
  final String name;

  /// Role in the room.
  final RtcRole role;

  /// Whether this is the participant using this device.
  final bool isLocal;

  /// `false` while the participant's connection is down and the server keeps
  /// its seat for a reconnect.
  final bool connected;

  /// Published tracks.
  final List<RtcTrack> tracks;

  /// Whether the participant is among the current active speakers.
  final bool isSpeaking;

  /// Track of [source], if published.
  RtcTrack? track(RtcTrackSource source) {
    for (final track in tracks) {
      if (track.source == source) return track;
    }
    return null;
  }

  /// Camera track, if published.
  RtcTrack? get camera => track(RtcTrackSource.camera);

  /// Screen share track, if published.
  RtcTrack? get screen => track(RtcTrackSource.screen);

  /// Whether the microphone is published and not muted.
  bool get microphoneOn => track(RtcTrackSource.microphone)?.muted == false;

  /// Whether the camera is published and not muted.
  bool get cameraOn => camera?.muted == false;

  /// Returns a copy with changed fields.
  RtcParticipant copyWith({bool? connected, List<RtcTrack>? tracks, bool? isSpeaking, String? name}) => RtcParticipant(
    id: id,
    identity: identity,
    name: name ?? this.name,
    role: role,
    isLocal: isLocal,
    connected: connected ?? this.connected,
    tracks: tracks ?? this.tracks,
    isSpeaking: isSpeaking ?? this.isSpeaking,
  );
}
