import 'package:ecochain_rtc/ecochain_rtc.dart';
import 'package:flutter_test/flutter_test.dart';

RtcParticipant person(String id, {bool camera = true, bool screen = false, bool cameraMuted = false}) => RtcParticipant(
  id: id,
  identity: id,
  name: id,
  role: RtcRole.participant,
  isLocal: false,
  connected: true,
  tracks: [
    if (camera) RtcTrack(id: '$id-cam', source: RtcTrackSource.camera, muted: cameraMuted, simulcast: true),
    if (screen) RtcTrack(id: '$id-scr', source: RtcTrackSource.screen, muted: false, simulcast: false),
  ],
);

void main() {
  final people = [for (var i = 0; i < 10; i++) person('p$i')];

  test('limits videos and keeps layout order', () {
    final requests = RtcVideoSubscriptionPolicy.select(
      remote: people,
      visibleIds: [for (final p in people) p.id],
      maxVideos: 4,
    );
    expect(requests.map((r) => r.trackId), ['p0-cam', 'p1-cam', 'p2-cam', 'p3-cam']);
    expect(requests.every((r) => r.quality == RtcVideoQuality.low), isTrue, reason: '10 tiles → thumbnails');
  });

  test('screen share, pinned and speakers come first', () {
    final remote = [...people, person('presenter', screen: true)];
    final requests = RtcVideoSubscriptionPolicy.select(
      remote: remote,
      visibleIds: ['p0', 'p1'],
      pinnedId: 'p7',
      speakerIds: ['p9'],
      maxVideos: 4,
    );
    expect(requests, const [
      RtcVideoRequest('presenter-scr', RtcVideoQuality.high),
      RtcVideoRequest('p7-cam', RtcVideoQuality.high),
      RtcVideoRequest('p9-cam', RtcVideoQuality.medium),
      RtcVideoRequest('p0-cam', RtcVideoQuality.medium),
    ]);
  });

  test('muted cameras do not take a slot and nothing is requested twice', () {
    final requests = RtcVideoSubscriptionPolicy.select(
      remote: [person('a', cameraMuted: true), person('b'), person('c')],
      visibleIds: ['a', 'b', 'b', 'c'],
      speakerIds: ['b'],
      maxVideos: 9,
    );
    expect(requests.map((r) => r.trackId), ['b-cam', 'c-cam']);
  });
}
