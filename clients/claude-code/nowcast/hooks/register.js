"use strict";
// The nowcast mod: one per Claude Code session. It declares the session to
// the service, holds the session's one event stream through `nowcast watch`,
// and writes each flow it receives into its state for other mods to read.
// Requests (subscribe, unsubscribe, refresh) go from each mod to the service:
// all mods run in this process, so the service ties them to the same session.
var __assign = (this && this.__assign) || function () {
    __assign = Object.assign || function(t) {
        for (var s, i = 1, n = arguments.length; i < n; i++) {
            s = arguments[i];
            for (var p in s) if (Object.prototype.hasOwnProperty.call(s, p))
                t[p] = s[p];
        }
        return t;
    };
    return __assign.apply(this, arguments);
};
var __awaiter = (this && this.__awaiter) || function (thisArg, _arguments, P, generator) {
    function adopt(value) { return value instanceof P ? value : new P(function (resolve) { resolve(value); }); }
    return new (P || (P = Promise))(function (resolve, reject) {
        function fulfilled(value) { try { step(generator.next(value)); } catch (e) { reject(e); } }
        function rejected(value) { try { step(generator["throw"](value)); } catch (e) { reject(e); } }
        function step(result) { result.done ? resolve(result.value) : adopt(result.value).then(fulfilled, rejected); }
        step((generator = generator.apply(thisArg, _arguments || [])).next());
    });
};
var __generator = (this && this.__generator) || function (thisArg, body) {
    var _ = { label: 0, sent: function() { if (t[0] & 1) throw t[1]; return t[1]; }, trys: [], ops: [] }, f, y, t, g = Object.create((typeof Iterator === "function" ? Iterator : Object).prototype);
    return g.next = verb(0), g["throw"] = verb(1), g["return"] = verb(2), typeof Symbol === "function" && (g[Symbol.iterator] = function() { return this; }), g;
    function verb(n) { return function (v) { return step([n, v]); }; }
    function step(op) {
        if (f) throw new TypeError("Generator is already executing.");
        while (g && (g = 0, op[0] && (_ = 0)), _) try {
            if (f = 1, y && (t = op[0] & 2 ? y["return"] : op[0] ? y["throw"] || ((t = y["return"]) && t.call(y), 0) : y.next) && !(t = t.call(y, op[1])).done) return t;
            if (y = 0, t) op = [op[0] & 2, t.value];
            switch (op[0]) {
                case 0: case 1: t = op; break;
                case 4: _.label++; return { value: op[1], done: false };
                case 5: _.label++; y = op[1]; op = [0]; continue;
                case 7: op = _.ops.pop(); _.trys.pop(); continue;
                default:
                    if (!(t = _.trys, t = t.length > 0 && t[t.length - 1]) && (op[0] === 6 || op[0] === 2)) { _ = 0; continue; }
                    if (op[0] === 3 && (!t || (op[1] > t[0] && op[1] < t[3]))) { _.label = op[1]; break; }
                    if (op[0] === 6 && _.label < t[1]) { _.label = t[1]; t = op; break; }
                    if (t && _.label < t[2]) { _.label = t[2]; _.ops.push(op); break; }
                    if (t[2]) _.ops.pop();
                    _.trys.pop(); continue;
            }
            op = body.call(thisArg, _);
        } catch (e) { op = [6, e]; y = 0; } finally { f = t = 0; }
        if (op[0] & 5) throw op[1]; return { value: op[0] ? op[1] : void 0, done: true };
    }
};
var __asyncValues = (this && this.__asyncValues) || function (o) {
    if (!Symbol.asyncIterator) throw new TypeError("Symbol.asyncIterator is not defined.");
    var m = o[Symbol.asyncIterator], i;
    return m ? m.call(o) : (o = typeof __values === "function" ? __values(o) : o[Symbol.iterator](), i = {}, verb("next"), verb("throw"), verb("return"), i[Symbol.asyncIterator] = function () { return this; }, i);
    function verb(n) { i[n] = o[n] && function (v) { return new Promise(function (resolve, reject) { v = o[n](v), settle(resolve, reject, v.done, v.value); }); }; }
    function settle(resolve, reject, d, v) { Promise.resolve(v).then(function(v) { resolve({ value: v, done: d }); }, reject); }
};
Object.defineProperty(exports, "__esModule", { value: true });
exports.register = void 0;
var stream_1 = require("./stream");
var FLOWS = { plugin: 'nowcast', key: 'flows' };
var STREAM = { plugin: 'nowcast', key: 'stream' };
/** The command line tool, looked up in the session's `PATH`. */
var CLI = 'nowcast';
/** Delay before starting `watch` again after it exits, doubled up to the max. */
var FIRST_RESTART_MS = 1000;
var MAX_RESTART_MS = 60000;
/** A `watch` that ran this long starts again after the first delay. */
var STABLE_MS = 60000;
/** After a `/clear` or a resume, how often to look for the new session id. */
var NEW_ID_POLL_MS = 50;
var NEW_ID_ATTEMPTS = 100;
// The running `watch`, and the session changes applied one at a time, in
// order. Both start over when the mod reloads, as `session.start` does.
var watch;
var queue = Promise.resolve();
// What this mod last wrote to its state. It is the only writer, and a read
// through the `$` of a long-lived loop would answer the moment that loop's
// dispatch began.
var flows = new Map();
var stream = { session: null, status: 'starting', reconnections: 0, error: null };
var register = function (on) {
    // Once per load of the mod: the session's start, or a reload. The start
    // does not wait for the service: `stream` says when the session is declared.
    on('session.start', function ($, e, next) { return __awaiter(void 0, void 0, void 0, function () {
        var started, _a, _b;
        return __generator(this, function (_c) {
            switch (_c.label) {
                case 0: return [4 /*yield*/, next(e)];
                case 1:
                    started = _c.sent();
                    _a = follow;
                    _b = [$];
                    return [4 /*yield*/, $.session.id()];
                case 2:
                    void _a.apply(void 0, _b.concat([_c.sent()]));
                    return [2 /*return*/, started];
            }
        });
    }); });
    // A `/clear` or a resume ends the session and the process goes on under a
    // new id, with no `session.start`: the new id shows once the end is done.
    on('session.end', function ($, e, next) { return __awaiter(void 0, void 0, void 0, function () {
        var ended;
        return __generator(this, function (_a) {
            switch (_a.label) {
                case 0: return [4 /*yield*/, next(e)];
                case 1:
                    ended = _a.sent();
                    if (e.reason === 'clear' || e.reason === 'resume')
                        awaitNewId($, e.sessionId, 0);
                    return [2 /*return*/, ended];
            }
        });
    }); });
    // A backstop: a turn always runs under the session the service knows.
    on('turn.start', function ($, e, next) { return __awaiter(void 0, void 0, void 0, function () {
        var session;
        return __generator(this, function (_a) {
            switch (_a.label) {
                case 0:
                    if (!(e.agentId === undefined)) return [3 /*break*/, 2];
                    return [4 /*yield*/, $.session.id()];
                case 1:
                    session = _a.sent();
                    if ((watch === null || watch === void 0 ? void 0 : watch.session) !== session)
                        void follow($, session);
                    _a.label = 2;
                case 2: return [2 /*return*/, next(e)];
            }
        });
    }); });
};
exports.register = register;
/** Follows the session that replaces `ended`, as soon as its id shows. */
function awaitNewId($, ended, attempt) {
    var _this = this;
    $.clock.after(NEW_ID_POLL_MS, function () { return __awaiter(_this, void 0, void 0, function () {
        var session;
        return __generator(this, function (_a) {
            switch (_a.label) {
                case 0: return [4 /*yield*/, $.session.id()];
                case 1:
                    session = _a.sent();
                    if (!(session !== ended)) return [3 /*break*/, 3];
                    return [4 /*yield*/, follow($, session)];
                case 2:
                    _a.sent();
                    return [3 /*break*/, 4];
                case 3:
                    if (attempt < NEW_ID_ATTEMPTS)
                        awaitNewId($, ended, attempt + 1);
                    _a.label = 4;
                case 4: return [2 /*return*/];
            }
        });
    }); });
}
/** Declares `session` and runs `watch` for it, unless it already runs. */
function follow($, session) {
    var _this = this;
    queue = queue
        .then(function () { return __awaiter(_this, void 0, void 0, function () {
        return __generator(this, function (_a) {
            switch (_a.label) {
                case 0:
                    if ((watch === null || watch === void 0 ? void 0 : watch.session) === session)
                        return [2 /*return*/];
                    watch === null || watch === void 0 ? void 0 : watch.stop();
                    watch = undefined;
                    // A new session starts with an empty state: the new `watch` sends the
                    // last value of every flow again, and each is written anew.
                    flows.clear();
                    return [4 /*yield*/, declare($, session)];
                case 1:
                    _a.sent();
                    watch = start($, session);
                    return [2 /*return*/];
            }
        });
    }); })
        .catch(function (error) { return $.ui.log("nowcast: cannot follow session ".concat(session, ": ").concat(message(error)), { to: 'debug' }); });
    return queue;
}
/** Where the service listens, as the `nowcast` command finds it. */
function socketPath($) {
    return __awaiter(this, void 0, void 0, function () {
        var explicit, runtime;
        return __generator(this, function (_a) {
            switch (_a.label) {
                case 0: return [4 /*yield*/, $.env.get('NOWCAST_SOCKET')];
                case 1:
                    explicit = _a.sent();
                    if (explicit)
                        return [2 /*return*/, explicit];
                    return [4 /*yield*/, $.env.get('XDG_RUNTIME_DIR')];
                case 2:
                    runtime = _a.sent();
                    return [2 /*return*/, (runtime === null || runtime === void 0 ? void 0 : runtime.startsWith('/')) ? "".concat(runtime, "/nowcast/nowcast.sock") : undefined];
            }
        });
    });
}
/**
 * `PUT /v1/session`. A service that does not answer is not an error here:
 * `watch` declares the session again each time it connects.
 */
function declare($, session) {
    return __awaiter(this, void 0, void 0, function () {
        var socket, answer, error_1;
        return __generator(this, function (_a) {
            switch (_a.label) {
                case 0: return [4 /*yield*/, socketPath($)];
                case 1:
                    socket = _a.sent();
                    if (socket === undefined) {
                        return [2 /*return*/, setStream($, { session: session, error: 'neither NOWCAST_SOCKET nor an absolute XDG_RUNTIME_DIR is set' })];
                    }
                    _a.label = 2;
                case 2:
                    _a.trys.push([2, 5, , 7]);
                    return [4 /*yield*/, $.http.fetch('http://nowcast/v1/session', {
                            method: 'PUT',
                            headers: { 'content-type': 'application/json' },
                            body: JSON.stringify({ id: session }),
                            socketPath: socket,
                        })];
                case 3:
                    answer = _a.sent();
                    return [4 /*yield*/, setStream($, answer.status === 204
                            ? { session: session, status: 'up', error: null }
                            : { session: session, error: "PUT /v1/session answered ".concat(answer.status, ": ").concat(answer.text) })];
                case 4:
                    _a.sent();
                    return [3 /*break*/, 7];
                case 5:
                    error_1 = _a.sent();
                    return [4 /*yield*/, setStream($, {
                            session: session,
                            status: stream.status === 'up' ? 'down' : stream.status,
                            error: "cannot reach the service at ".concat(socket, ": ").concat(message(error_1)),
                        })];
                case 6:
                    _a.sent();
                    return [3 /*break*/, 7];
                case 7: return [2 /*return*/];
            }
        });
    });
}
/** Runs `watch` for `session` until stopped, starting it again if it exits. */
function start($, session) {
    var _this = this;
    var child;
    var stopped = false;
    void (function () { return __awaiter(_this, void 0, void 0, function () {
        var delay, startedAt, why;
        return __generator(this, function (_a) {
            switch (_a.label) {
                case 0:
                    delay = FIRST_RESTART_MS;
                    _a.label = 1;
                case 1: return [4 /*yield*/, $.clock.now()];
                case 2:
                    startedAt = _a.sent();
                    if (stopped)
                        return [2 /*return*/];
                    child = $.process.spawn({ argv: [CLI, '--json', '--session', session, 'watch'] });
                    return [4 /*yield*/, run($, child, function () { return stopped; })];
                case 3:
                    why = _a.sent();
                    if (stopped)
                        return [2 /*return*/];
                    return [4 /*yield*/, setStream($, { status: 'stopped', error: why })];
                case 4:
                    _a.sent();
                    $.ui.log("nowcast: ".concat(why), { to: 'debug' });
                    return [4 /*yield*/, $.clock.now()];
                case 5:
                    if ((_a.sent()) - startedAt >= STABLE_MS)
                        delay = FIRST_RESTART_MS;
                    return [4 /*yield*/, $.clock.sleep(delay)];
                case 6:
                    _a.sent();
                    delay = Math.min(delay * 2, MAX_RESTART_MS);
                    _a.label = 7;
                case 7: return [3 /*break*/, 1];
                case 8: return [2 /*return*/];
            }
        });
    }); })().catch(function () {
        // The mod is unloading: its calls on `$` are refused from now on.
    });
    return {
        session: session,
        stop: function () {
            stopped = true;
            void (child === null || child === void 0 ? void 0 : child.return({ code: null, signal: null }));
        },
    };
}
/**
 * Reads one `watch` until it ends, and says why it ended. Once `stopped`, what
 * the stopped `watch` still prints belongs to the session left behind: it is
 * dropped.
 */
function run($, child, stopped) {
    return __awaiter(this, void 0, void 0, function () {
        var stdout, stderr, lastError, stream_2, text, _i, _a, line, _b, _c, line, e_1_1, _d, code, signal, status_1, error_2;
        var _e, child_1, child_1_1;
        var _f, e_1, _g, _h;
        var _j;
        return __generator(this, function (_k) {
            switch (_k.label) {
                case 0:
                    stdout = new stream_1.Lines();
                    stderr = new stream_1.Lines();
                    lastError = '';
                    _k.label = 1;
                case 1:
                    _k.trys.push([1, 23, , 24]);
                    _k.label = 2;
                case 2:
                    _k.trys.push([2, 15, 16, 21]);
                    _e = true, child_1 = __asyncValues(child);
                    _k.label = 3;
                case 3: return [4 /*yield*/, child_1.next()];
                case 4:
                    if (!(child_1_1 = _k.sent(), _f = child_1_1.done, !_f)) return [3 /*break*/, 14];
                    _h = child_1_1.value;
                    _e = false;
                    stream_2 = _h.stream, text = _h.text;
                    if (stopped())
                        return [3 /*break*/, 14];
                    if (!(stream_2 === 'stdout')) return [3 /*break*/, 9];
                    _i = 0, _a = stdout.push(text);
                    _k.label = 5;
                case 5:
                    if (!(_i < _a.length)) return [3 /*break*/, 8];
                    line = _a[_i];
                    if (!!stopped()) return [3 /*break*/, 7];
                    return [4 /*yield*/, onEvent($, line)];
                case 6:
                    _k.sent();
                    _k.label = 7;
                case 7:
                    _i++;
                    return [3 /*break*/, 5];
                case 8: return [3 /*break*/, 13];
                case 9:
                    _b = 0, _c = stderr.push(text);
                    _k.label = 10;
                case 10:
                    if (!(_b < _c.length)) return [3 /*break*/, 13];
                    line = _c[_b];
                    if (stopped())
                        return [3 /*break*/, 13];
                    lastError = ((_j = (0, stream_1.parseConnection)(line)) === null || _j === void 0 ? void 0 : _j.status) === 'up' ? '' : errorText(line);
                    return [4 /*yield*/, onStderr($, line)];
                case 11:
                    _k.sent();
                    _k.label = 12;
                case 12:
                    _b++;
                    return [3 /*break*/, 10];
                case 13:
                    _e = true;
                    return [3 /*break*/, 3];
                case 14: return [3 /*break*/, 21];
                case 15:
                    e_1_1 = _k.sent();
                    e_1 = { error: e_1_1 };
                    return [3 /*break*/, 21];
                case 16:
                    _k.trys.push([16, , 19, 20]);
                    if (!(!_e && !_f && (_g = child_1.return))) return [3 /*break*/, 18];
                    return [4 /*yield*/, _g.call(child_1)];
                case 17:
                    _k.sent();
                    _k.label = 18;
                case 18: return [3 /*break*/, 20];
                case 19:
                    if (e_1) throw e_1.error;
                    return [7 /*endfinally*/];
                case 20: return [7 /*endfinally*/];
                case 21: return [4 /*yield*/, child.result];
                case 22:
                    _d = _k.sent(), code = _d.code, signal = _d.signal;
                    status_1 = signal !== null && signal !== void 0 ? signal : "status ".concat(code);
                    return [2 /*return*/, lastError ? "".concat(CLI, " watch exited (").concat(status_1, "): ").concat(lastError) : "".concat(CLI, " watch exited (").concat(status_1, ")")];
                case 23:
                    error_2 = _k.sent();
                    return [2 /*return*/, "cannot run ".concat(CLI, ": ").concat(message(error_2))];
                case 24: return [2 /*return*/];
            }
        });
    });
}
function onEvent($, line) {
    return __awaiter(this, void 0, void 0, function () {
        var event, next;
        return __generator(this, function (_a) {
            switch (_a.label) {
                case 0:
                    event = (0, stream_1.parseEvent)(line);
                    if (event === null)
                        return [2 /*return*/];
                    next = (0, stream_1.nextFlow)(flows.get(event.flow), event);
                    if (next === null)
                        return [2 /*return*/];
                    flows.set(event.flow, next);
                    return [4 /*yield*/, write($, function () { return $.state.set(__assign(__assign({}, FLOWS), { id: event.flow }), next); }, "flow ".concat(event.flow))];
                case 1:
                    _a.sent();
                    return [2 /*return*/];
            }
        });
    });
}
function onStderr($, line) {
    return __awaiter(this, void 0, void 0, function () {
        var connection, reconnections;
        return __generator(this, function (_a) {
            switch (_a.label) {
                case 0:
                    connection = (0, stream_1.parseConnection)(line);
                    if (!(connection === null)) return [3 /*break*/, 1];
                    $.ui.log(line, { to: 'debug' });
                    return [3 /*break*/, 5];
                case 1:
                    if (!(connection.status === 'up')) return [3 /*break*/, 3];
                    reconnections = stream.status === 'down' ? stream.reconnections + 1 : stream.reconnections;
                    return [4 /*yield*/, setStream($, { status: 'up', reconnections: reconnections, error: null })];
                case 2:
                    _a.sent();
                    return [3 /*break*/, 5];
                case 3: return [4 /*yield*/, setStream($, { status: 'down', error: connection.reason })];
                case 4:
                    _a.sent();
                    _a.label = 5;
                case 5: return [2 /*return*/];
            }
        });
    });
}
function setStream($, change) {
    return __awaiter(this, void 0, void 0, function () {
        var value;
        return __generator(this, function (_a) {
            switch (_a.label) {
                case 0:
                    stream = __assign(__assign({}, stream), change);
                    value = stream;
                    return [4 /*yield*/, write($, function () { return $.state.set(STREAM, value); }, 'stream')];
                case 1:
                    _a.sent();
                    return [2 /*return*/];
            }
        });
    });
}
/**
 * A write to the state. One another mod refuses (its `state.set` hook may)
 * is logged and skipped: it must not end the stream, which would only replay
 * the same value into the same refusal.
 */
function write($, set, what) {
    return __awaiter(this, void 0, void 0, function () {
        var error_3;
        return __generator(this, function (_a) {
            switch (_a.label) {
                case 0:
                    _a.trys.push([0, 2, , 3]);
                    return [4 /*yield*/, set()];
                case 1:
                    _a.sent();
                    return [3 /*break*/, 3];
                case 2:
                    error_3 = _a.sent();
                    $.ui.log("nowcast: cannot write ".concat(what, ": ").concat(message(error_3)), { to: 'debug' });
                    return [3 /*break*/, 3];
                case 3: return [2 /*return*/];
            }
        });
    });
}
/** A fatal error's message from `--json` output, or the line as it is. */
function errorText(line) {
    var _a;
    try {
        var parsed = JSON.parse(line);
        if (typeof ((_a = parsed.error) === null || _a === void 0 ? void 0 : _a.message) === 'string')
            return parsed.error.message;
    }
    catch (_b) {
        // Not JSON: a line meant for people.
    }
    return line;
}
function message(error) {
    return error instanceof Error ? error.message : String(error);
}
