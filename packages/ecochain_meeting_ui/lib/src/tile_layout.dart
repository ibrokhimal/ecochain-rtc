import 'dart:math';
import 'dart:ui';

/// Sizes the gallery: the arrangement that shows the biggest pictures without
/// scrolling, or a scrolling grid once tiles would get too small.
///
/// A wide area uses 16:9 tiles. A tall one (a phone held upright) stretches
/// the tiles to fill the screen, since most pictures there are upright. A
/// single tile takes the whole area.
({int columns, Size tile, bool scrolls}) tileGrid({required int count, required Size area, double gap = 8}) {
  if (count <= 1 || area.isEmpty) return (columns: 1, tile: area, scrolls: false);
  final wide = area.width >= area.height;
  final minAspect = wide ? 16 / 9 : 9 / 16;
  final maxAspect = wide ? 16 / 9 : 4 / 3;
  // A phone always gets two per row, however narrow.
  final minWidth = min(160.0, (area.width - gap) / 2);
  final maxColumns = min(count, ((area.width + gap) / (minWidth + gap) + 0.001).floor());

  Size? best;
  var columns = 1;
  var bestScore = 0.0;
  for (var c = 1; c <= maxColumns; c++) {
    final rows = (count / c).ceil();
    final cell = Size((area.width - gap * (c - 1)) / c, (area.height - gap * (rows - 1)) / rows);
    if (cell.height <= 0) continue;
    final tile = Size(min(cell.width, cell.height * maxAspect), min(cell.height, cell.width / minAspect));
    // Height a typical picture (16:9, or 3:4 on an upright phone) gets.
    final score = min(tile.height, tile.width / (wide ? 16 / 9 : 3 / 4));
    if (score > bestScore) {
      bestScore = score;
      best = tile;
      columns = c;
    }
  }
  if (best != null && best.width >= minWidth) return (columns: columns, tile: best, scrolls: false);
  final width = min((area.width - gap * (maxColumns - 1)) / maxColumns, max(area.height, 90) * maxAspect);
  return (columns: maxColumns, tile: Size(width, width / maxAspect), scrolls: true);
}

/// Places [count] tiles of a grid that does not scroll, centred in [area];
/// an incomplete last row is centred too.
List<Rect> tileRects({
  required int count,
  required Size area,
  required int columns,
  required Size tile,
  double gap = 8,
}) {
  final rows = (count / columns).ceil();
  final top = (area.height - rows * tile.height - (rows - 1) * gap) / 2;
  return [
    for (var i = 0; i < count; i++)
      () {
        final row = i ~/ columns;
        final inRow = min(columns, count - row * columns);
        final left = (area.width - inRow * tile.width - (inRow - 1) * gap) / 2;
        return Offset(left + (i % columns) * (tile.width + gap), top + row * (tile.height + gap)) & tile;
      }(),
  ];
}

/// Size of the small floating picture in a two-person call.
Size floatingTileSize(Size area) {
  final aspect = area.width >= area.height ? 16 / 9 : 3 / 4;
  final width = (area.shortestSide * 0.3).clamp(96.0, 180.0) * (aspect > 1 ? 4 / 3 : 1);
  final height = min(width / aspect, area.height * 0.4);
  return Size(height * aspect, height);
}
