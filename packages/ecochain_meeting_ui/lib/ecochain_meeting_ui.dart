/// Shared Ecochain meeting interface for the standalone Meet app and for
/// apps that embed meetings (Messenger).
///
/// * [MeetingView]: the in-call screen for an existing `RtcSession`.
/// * [JoinPanel]: server URL + token form with a development token helper.
/// * [meetingTheme]: the Ecochain color scheme.
library;

import 'package:flutter/material.dart';

export 'src/dev_token.dart';
export 'src/join_panel.dart';
export 'src/meeting_view.dart' show MeetingView, ParticipantTile;
export 'src/strings.dart';

/// Ecochain Material 3 theme used by both demo apps.
ThemeData meetingTheme() => ThemeData(
  colorScheme: ColorScheme.fromSeed(seedColor: const Color(0xff00776d)),
  scaffoldBackgroundColor: const Color(0xfff4f8f7),
  useMaterial3: true,
);
