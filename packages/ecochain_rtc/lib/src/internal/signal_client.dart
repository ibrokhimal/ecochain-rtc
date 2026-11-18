import 'dart:async';
import 'dart:convert';
import 'dart:math';

import 'package:web_socket_channel/web_socket_channel.dart';

import '../errors.dart';

/// Protocol version spoken by this SDK.
const int protocolVersion = 1;

/// Server push event (a frame without a request id).
class SignalEvent {
  /// Creates an event.
  const SignalEvent(this.type, this.data);

  /// Event type, for example `participant.joined`.
  final String type;

  /// Event payload.
  final Map<String, dynamic> data;
}

/// WebSocket close codes the server uses (see `docs/PROTOCOL.md`).
abstract final class CloseCodes {
  /// Normal close after `leave`.
  static const normal = 1000;

  /// Protocol error.
  static const protocolError = 4000;

  /// Another connection of the same identity took over.
  static const sessionReplaced = 4001;

  /// Token rejected.
  static const unauthorized = 4003;

  /// Meeting ended by the host.
  static const roomEnded = 4004;

  /// Removed by the host.
  static const removed = 4005;
}

/// One signaling WebSocket: request/response matching by id, push events
/// and heartbeat.
///
/// A [SignalClient] is used for exactly one socket. Reconnecting creates a
/// new instance, so late frames of an old socket can never be mistaken for
/// answers on the new one.
class SignalClient {
  SignalClient._(this._channel, this._timeout);

  final WebSocketChannel _channel;
  final Duration _timeout;
  final _pending = <String, Completer<Map<String, dynamic>>>{};
  final _events = StreamController<SignalEvent>.broadcast(sync: true);
  final _closed = Completer<int?>();
  StreamSubscription<dynamic>? _subscription;
  Timer? _heartbeat;
  DateTime _lastReceived = DateTime.now();
  int _nextId = 1;

  /// Random per-socket prefix. The server remembers request ids for the
  /// whole session, across resumes, so ids must not restart at `r1` on a new
  /// socket: a reused id would be answered from the cache without running.
  final String _idPrefix = 'r${Random.secure().nextInt(1 << 32).toRadixString(36)}-';

  /// Push events from the server.
  Stream<SignalEvent> get events => _events.stream;

  /// Completes with the close code when the socket closes for any reason
  /// (`null` when the transport failed without a close frame).
  Future<int?> get closed => _closed.future;

  /// Whether the socket is closed.
  bool get isClosed => _closed.isCompleted;

  /// Opens a socket to [url].
  ///
  /// Throws [RtcException] with [RtcErrorCode.connectionLost] if the socket
  /// cannot be opened within [timeout].
  static Future<SignalClient> connect(Uri url, Duration timeout) async {
    final channel = WebSocketChannel.connect(url);
    try {
      await channel.ready.timeout(timeout);
    } catch (e) {
      unawaited(channel.sink.close().catchError((_) {}));
      throw RtcException(RtcErrorCode.connectionLost, 'cannot open signaling socket: ${e.runtimeType}');
    }
    final client = SignalClient._(channel, timeout);
    client._subscription = channel.stream.listen(
      client._onFrame,
      onDone: () => client._finish(channel.closeCode),
      onError: (Object _) => client._finish(null),
      cancelOnError: true,
    );
    return client;
  }

  /// Sends a request and waits for its response data.
  ///
  /// Pass the same [id] when retrying a request after a reconnect: the
  /// server recognizes it and returns the first result instead of executing
  /// the request twice.
  ///
  /// Throws [RtcException]: the server's error code, [RtcErrorCode.timeout]
  /// or [RtcErrorCode.connectionLost].
  Future<Map<String, dynamic>> request(String type, [Map<String, dynamic>? data, String? id]) {
    if (isClosed) {
      return Future.error(const RtcException(RtcErrorCode.connectionLost, 'signaling socket is closed'));
    }
    final requestId = id ?? '$_idPrefix${_nextId++}';
    final completer = Completer<Map<String, dynamic>>();
    _pending[requestId] = completer;
    _channel.sink.add(jsonEncode({'v': protocolVersion, 'type': type, 'id': requestId, 'data': data ?? const {}}));
    return completer.future.timeout(
      _timeout,
      onTimeout: () {
        _pending.remove(requestId);
        throw RtcException(RtcErrorCode.timeout, '$type timed out');
      },
    );
  }

  /// Starts sending `ping` every [interval]. If nothing arrives for
  /// [deadAfter] the socket is closed, which [closed] reports.
  ///
  /// Pings matter on mobile: a socket over a dead network often stays "open"
  /// for minutes until TCP gives up.
  void startHeartbeat(Duration interval, Duration deadAfter) {
    _heartbeat?.cancel();
    _heartbeat = Timer.periodic(interval, (_) {
      if (DateTime.now().difference(_lastReceived) > deadAfter) {
        close();
        return;
      }
      request('ping').ignore();
    });
  }

  /// Closes the socket. Pending requests fail with
  /// [RtcErrorCode.connectionLost]. Safe to call more than once.
  Future<void> close() async {
    _heartbeat?.cancel();
    await _channel.sink.close().catchError((_) {});
    _finish(null);
  }

  void _onFrame(dynamic frame) {
    _lastReceived = DateTime.now();
    if (frame is! String) return;
    final Map<String, dynamic> message;
    try {
      message = jsonDecode(frame) as Map<String, dynamic>;
    } on FormatException {
      return;
    }
    final type = message['type'] as String?;
    if (type == 'response') {
      final completer = _pending.remove(message['id']);
      if (completer == null) return;
      if (message['ok'] == true) {
        completer.complete((message['data'] as Map<String, dynamic>?) ?? const {});
      } else {
        final error = (message['error'] as Map<String, dynamic>?) ?? const {};
        completer.completeError(
          RtcException(RtcErrorCode.fromServer(error['code'] as String?), (error['message'] as String?) ?? 'error'),
        );
      }
    } else if (type != null) {
      _events.add(SignalEvent(type, (message['data'] as Map<String, dynamic>?) ?? const {}));
    }
  }

  void _finish(int? code) {
    if (_closed.isCompleted) return;
    _heartbeat?.cancel();
    _subscription?.cancel();
    for (final completer in _pending.values) {
      completer.completeError(const RtcException(RtcErrorCode.connectionLost, 'signaling socket closed'));
    }
    _pending.clear();
    _closed.complete(code);
    _events.close();
  }
}
