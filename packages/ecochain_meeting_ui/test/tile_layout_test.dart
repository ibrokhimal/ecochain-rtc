import 'dart:ui';

import 'package:ecochain_meeting_ui/src/tile_layout.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('a wide area uses 16:9 tiles', () {
    for (final n in [2, 3, 4, 9]) {
      expect(tileGrid(count: n, area: const Size(1280, 640)).tile.aspectRatio, closeTo(16 / 9, 0.001));
    }
    expect(tileGrid(count: 4, area: const Size(1280, 640)).columns, 2);
  });

  test('an upright phone fills the screen with two per row', () {
    for (final n in [3, 4, 5, 6, 8]) {
      final g = tileGrid(count: n, area: const Size(374, 640));
      expect(g.columns, 2);
      expect(g.scrolls, isFalse);
      expect(g.tile.width, 183);
    }
    expect(tileGrid(count: 30, area: const Size(374, 640)).scrolls, isTrue);
  });

  test('tiles stay inside the area and do not overlap', () {
    for (final area in const [Size(304, 400), Size(374, 640), Size(828, 200), Size(1264, 640), Size(800, 1100)]) {
      for (var n = 1; n <= 100; n++) {
        final g = tileGrid(count: n, area: area);
        expect(g.columns * (g.tile.width + 8) - 8, lessThanOrEqualTo(area.width + 0.001));
        if (g.scrolls) continue;
        final rects = tileRects(count: n, area: area, columns: g.columns, tile: g.tile);
        for (final (i, r) in rects.indexed) {
          expect(r.left >= -0.001 && r.top >= -0.001, isTrue);
          expect(r.right <= area.width + 0.001 && r.bottom <= area.height + 0.001, isTrue);
          for (final other in rects.skip(i + 1)) {
            expect(r.overlaps(other), isFalse);
          }
        }
      }
    }
  });
}
