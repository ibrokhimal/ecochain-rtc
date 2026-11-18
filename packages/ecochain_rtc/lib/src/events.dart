import 'connection_state.dart';
import 'participant.dart';

/// Something that happened in a session. Listen on [RtcSession.events].
///
/// ```dart
/// session.events.listen((event) {
///   switch (event) {
///     case RtcParticipantJoined(:final participant):
///       showToast('${participant.name} joined');
///     case RtcSessionEnded(:final reason):
///       if (reason == RtcDisconnectReason.roomEnded) showEndedBanner();
///     default:
///   }
/// });
/// ```
///
/// For rebuilding UI, listening to the session itself (it is a
/// `ChangeNotifier`) is usually simpler; events are for one-off reactions.
sealed class RtcEvent {
  const RtcEvent();
}

/// The connection state changed.
class RtcConnectionStateChanged extends RtcEvent {
  /// Creates the event.
  const RtcConnectionStateChanged(this.state);

  /// New state.
  final RtcConnectionState state;
}

/// A remote participant joined.
class RtcParticipantJoined extends RtcEvent {
  /// Creates the event.
  const RtcParticipantJoined(this.participant);

  /// The participant.
  final RtcParticipant participant;
}

/// A remote participant changed (tracks, mute state, connectivity).
class RtcParticipantUpdated extends RtcEvent {
  /// Creates the event.
  const RtcParticipantUpdated(this.participant);

  /// The new snapshot.
  final RtcParticipant participant;
}

/// A remote participant left.
class RtcParticipantLeft extends RtcEvent {
  /// Creates the event.
  const RtcParticipantLeft(this.participantId, this.reason);

  /// Id of the participant that left.
  final String participantId;

  /// Server reason: `left`, `timeout`, `removed`.
  final String reason;
}

/// Media of a remote track started arriving on this device.
class RtcTrackSubscribed extends RtcEvent {
  /// Creates the event.
  const RtcTrackSubscribed(this.participantId, this.track);

  /// Publisher.
  final String participantId;

  /// The track, with [RtcTrack.mediaTrack] set.
  final RtcTrack track;
}

/// Active speakers changed (loudest first).
class RtcActiveSpeakersChanged extends RtcEvent {
  /// Creates the event.
  const RtcActiveSpeakersChanged(this.participantIds);

  /// Speaking participants, loudest first, at most three.
  final List<String> participantIds;
}

/// The session ended. No further events follow.
class RtcSessionEnded extends RtcEvent {
  /// Creates the event.
  const RtcSessionEnded(this.reason);

  /// Why it ended.
  final RtcDisconnectReason reason;
}
