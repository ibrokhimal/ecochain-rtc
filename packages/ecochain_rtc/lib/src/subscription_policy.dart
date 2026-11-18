import 'participant.dart';

/// Requested receive quality of a video.
enum RtcVideoQuality {
  /// Thumbnails.
  low,

  /// Gallery tiles.
  medium,

  /// Spotlight, pinned video, screen share.
  high,
}

/// One video this device wants to receive.
class RtcVideoRequest {
  /// Creates a request.
  const RtcVideoRequest(this.trackId, this.quality);

  /// Track to receive.
  final String trackId;

  /// Desired quality.
  final RtcVideoQuality quality;

  @override
  bool operator ==(Object other) => other is RtcVideoRequest && other.trackId == trackId && other.quality == quality;

  @override
  int get hashCode => Object.hash(trackId, quality);

  @override
  String toString() => 'RtcVideoRequest($trackId, ${quality.name})';
}

/// Decides which remote videos to receive when more are published than
/// the device should decode.
///
/// The room may hold 25 people, but a phone should decode only a few
/// videos. Priority, highest first:
///
/// 1. screen shares (high quality),
/// 2. the pinned participant (high),
/// 3. active speakers that are visible or off screen (medium),
/// 4. other visible participants in layout order (medium for up to four
///    tiles, low for larger galleries).
///
/// Muted cameras are skipped: they would only cost a slot.
///
/// ```dart
/// final requests = RtcVideoSubscriptionPolicy.select(
///   remote: session.remoteParticipants,
///   visibleIds: ['PA_1', 'PA_2'],
///   pinnedId: 'PA_2',
///   speakerIds: session.activeSpeakerIds,
///   maxVideos: 4,
/// );
/// ```
abstract final class RtcVideoSubscriptionPolicy {
  /// Returns at most [maxVideos] requests in priority order.
  static List<RtcVideoRequest> select({
    required List<RtcParticipant> remote,
    required List<String> visibleIds,
    String? pinnedId,
    List<String> speakerIds = const [],
    required int maxVideos,
  }) {
    final result = <RtcVideoRequest>[];
    final chosen = <String>{};
    void add(RtcTrack? track, RtcVideoQuality quality) {
      if (track == null || track.muted || result.length >= maxVideos || !chosen.add(track.id)) return;
      result.add(RtcVideoRequest(track.id, quality));
    }

    RtcParticipant? byId(String id) {
      for (final p in remote) {
        if (p.id == id) return p;
      }
      return null;
    }

    for (final p in remote) {
      add(p.screen, RtcVideoQuality.high);
    }
    if (pinnedId != null) add(byId(pinnedId)?.camera, RtcVideoQuality.high);
    for (final id in speakerIds) {
      add(byId(id)?.camera, RtcVideoQuality.medium);
    }
    final gallery = visibleIds.length <= 4 ? RtcVideoQuality.medium : RtcVideoQuality.low;
    for (final id in visibleIds) {
      add(byId(id)?.camera, gallery);
    }
    return result;
  }
}
