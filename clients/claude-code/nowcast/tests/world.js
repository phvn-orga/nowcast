"use strict";
// A fake world beneath the mod: the session, the environment, a nowcast
// service answering `PUT /v1/session`, and `nowcast watch` processes the test
// writes to and ends.
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
var __await = (this && this.__await) || function (v) { return this instanceof __await ? (this.v = v, this) : new __await(v); }
var __asyncGenerator = (this && this.__asyncGenerator) || function (thisArg, _arguments, generator) {
    if (!Symbol.asyncIterator) throw new TypeError("Symbol.asyncIterator is not defined.");
    var g = generator.apply(thisArg, _arguments || []), i, q = [];
    return i = Object.create((typeof AsyncIterator === "function" ? AsyncIterator : Object).prototype), verb("next"), verb("throw"), verb("return", awaitReturn), i[Symbol.asyncIterator] = function () { return this; }, i;
    function awaitReturn(f) { return function (v) { return Promise.resolve(v).then(f, reject); }; }
    function verb(n, f) { if (g[n]) { i[n] = function (v) { return new Promise(function (a, b) { q.push([n, v, a, b]) > 1 || resume(n, v); }); }; if (f) i[n] = f(i[n]); } }
    function resume(n, v) { try { step(g[n](v)); } catch (e) { settle(q[0][3], e); } }
    function step(r) { r.value instanceof __await ? Promise.resolve(r.value.v).then(fulfill, reject) : settle(q[0][2], r); }
    function fulfill(value) { resume("next", value); }
    function reject(value) { resume("throw", value); }
    function settle(f, v) { if (f(v), q.shift(), q.length) resume(q[0][0], q[0][1]); }
};
var __asyncValues = (this && this.__asyncValues) || function (o) {
    if (!Symbol.asyncIterator) throw new TypeError("Symbol.asyncIterator is not defined.");
    var m = o[Symbol.asyncIterator], i;
    return m ? m.call(o) : (o = typeof __values === "function" ? __values(o) : o[Symbol.iterator](), i = {}, verb("next"), verb("throw"), verb("return"), i[Symbol.asyncIterator] = function () { return this; }, i);
    function verb(n) { i[n] = o[n] && function (v) { return new Promise(function (resolve, reject) { v = o[n](v), settle(resolve, reject, v.done, v.value); }); }; }
    function settle(resolve, reject, d, v) { Promise.resolve(v).then(function(v) { resolve({ value: v, done: d }); }, reject); }
};
var __asyncDelegator = (this && this.__asyncDelegator) || function (o) {
    var i, p;
    return i = {}, verb("next"), verb("throw", function (e) { throw e; }), verb("return"), i[Symbol.iterator] = function () { return this; }, i;
    function verb(n, f) { i[n] = o[n] ? function (v) { return (p = !p) ? { value: __await(o[n](v)), done: false } : f ? f(v) : v; } : f; }
};
var __values = (this && this.__values) || function(o) {
    var s = typeof Symbol === "function" && Symbol.iterator, m = s && o[s], i = 0;
    if (m) return m.call(o);
    if (o && typeof o.length === "number") return {
        next: function () {
            if (o && i >= o.length) o = void 0;
            return { value: o && o[i++], done: !o };
        }
    };
    throw new TypeError(s ? "Object is not iterable." : "Symbol.iterator is not defined.");
};
Object.defineProperty(exports, "__esModule", { value: true });
exports.observer = exports.START = exports.Child = exports.SOCKET = void 0;
exports.world = world;
exports.update = update;
var testing_1 = require("claude-code/testing");
exports.SOCKET = '/run/user/1000/nowcast/nowcast.sock';
/** One process the mod started, which the test drives. */
var Child = /** @class */ (function () {
    function Child(argv) {
        /** Set when the mod stopped it, rather than it ending on its own. */
        this.killed = false;
        this.pending = [];
        this.ended = false;
        this.argv = argv;
    }
    Object.defineProperty(Child.prototype, "session", {
        /** The value given after `--session`. */
        get: function () {
            return this.argv[this.argv.indexOf('--session') + 1];
        },
        enumerable: false,
        configurable: true
    });
    Child.prototype.stdout = function (text) {
        this.push({ stream: 'stdout', text: text });
    };
    Child.prototype.stderr = function (text) {
        this.push({ stream: 'stderr', text: text });
    };
    /** One `watch --json` line on stdout. */
    Child.prototype.event = function (event, data) {
        this.stdout("".concat(JSON.stringify({ event: event, data: data }), "\n"));
    };
    Child.prototype.exit = function (code, signal) {
        if (signal === void 0) { signal = null; }
        this.push({ code: code, signal: signal });
    };
    Object.defineProperty(Child.prototype, "running", {
        get: function () {
            return !this.ended && !this.killed;
        },
        enumerable: false,
        configurable: true
    });
    Child.prototype.run = function (signal) {
        return __asyncGenerator(this, arguments, function run_1() {
            var aborted, item;
            var _this = this;
            return __generator(this, function (_a) {
                switch (_a.label) {
                    case 0:
                        aborted = new Promise(function (resolve) { return signal.addEventListener('abort', function () { return resolve(); }); });
                        _a.label = 1;
                    case 1:
                        _a.trys.push([1, , 13, 14]);
                        _a.label = 2;
                    case 2:
                        item = this.pending.shift();
                        if (!(item === undefined)) return [3 /*break*/, 6];
                        return [4 /*yield*/, __await(Promise.race([new Promise(function (resolve) { return (_this.wake = resolve); }), aborted]))];
                    case 3:
                        _a.sent();
                        if (!signal.aborted) return [3 /*break*/, 5];
                        this.killed = true;
                        return [4 /*yield*/, __await({ code: null, signal: 'SIGTERM' })];
                    case 4: return [2 /*return*/, _a.sent()];
                    case 5: return [3 /*break*/, 11];
                    case 6:
                        if (!('stream' in item)) return [3 /*break*/, 9];
                        return [4 /*yield*/, __await(item)];
                    case 7: return [4 /*yield*/, _a.sent()];
                    case 8:
                        _a.sent();
                        return [3 /*break*/, 11];
                    case 9:
                        this.ended = true;
                        return [4 /*yield*/, __await(item)];
                    case 10: return [2 /*return*/, _a.sent()];
                    case 11: return [3 /*break*/, 2];
                    case 12: return [3 /*break*/, 14];
                    case 13:
                        if (!this.ended)
                            this.killed = true;
                        return [7 /*endfinally*/];
                    case 14: return [2 /*return*/];
                }
            });
        });
    };
    Child.prototype.push = function (item) {
        var _a;
        this.pending.push(item);
        (_a = this.wake) === null || _a === void 0 ? void 0 : _a.call(this);
    };
    return Child;
}());
exports.Child = Child;
function world(on, options) {
    var _a;
    if (options === void 0) { options = {}; }
    var clock = testing_1.mock.clock(on, { now: 1000000 });
    testing_1.mock.env(on, (_a = options.env) !== null && _a !== void 0 ? _a : { XDG_RUNTIME_DIR: '/run/user/1000' });
    var requests = [];
    var children = [];
    var session = { id: 'session-1' };
    on('session.id', function () { return ({ value: session.id }); });
    on('session.start', function (_, e) { return ({ cwd: e.cwd }); });
    on('session.end', function (_, e) { return ({ sessionId: e.sessionId }); });
    on('turn.start', function (_, e) { return ({ turnId: e.turnId }); });
    var writes = [];
    var observed = [];
    on('ui.log', function (_, e) {
        if (e.text.startsWith('observer '))
            observed.push(e.text.slice('observer '.length));
        return { value: undefined };
    });
    on('state.set', { plugin: 'nowcast', key: 'flows' }, function (_, e, next) {
        if (e.id === options.refuse)
            return { deny: 'refused by an observer' };
        writes.push({ key: e.key, id: e.id, value: e.value });
        return next(e);
    });
    on('state.set', { plugin: 'nowcast', key: 'stream' }, function (_, e, next) {
        writes.push({ key: e.key, id: undefined, value: e.value });
        return next(e);
    });
    on('http.fetch', function (_, e) {
        var _a, _b, _c, _d, _e, _f, _g;
        var request = {
            method: (_b = (_a = e.init) === null || _a === void 0 ? void 0 : _a.method) !== null && _b !== void 0 ? _b : 'GET',
            url: e.url,
            body: (_c = e.init) === null || _c === void 0 ? void 0 : _c.body,
            socketPath: (_d = e.init) === null || _d === void 0 ? void 0 : _d.socketPath,
        };
        requests.push(request);
        var answer = (_f = (_e = options.answer) === null || _e === void 0 ? void 0 : _e.call(options, request)) !== null && _f !== void 0 ? _f : {};
        if (typeof answer === 'string')
            return { deny: answer };
        var status = (_g = answer.status) !== null && _g !== void 0 ? _g : 204;
        return { value: __assign({ status: status, ok: status < 300, headers: {}, text: '' }, answer) };
    });
    on('process.spawn', function (_, e, next) {
        return __asyncGenerator(this, arguments, function () {
            var child;
            var _a;
            return __generator(this, function (_b) {
                switch (_b.label) {
                    case 0:
                        if (!options.spawnFails) return [3 /*break*/, 2];
                        return [4 /*yield*/, __await({ deny: "".concat(e.argv[0], ": not found") })];
                    case 1: return [2 /*return*/, _b.sent()];
                    case 2:
                        child = new Child(e.argv);
                        children.push(child);
                        _a = {};
                        return [5 /*yield**/, __values(__asyncDelegator(__asyncValues(child.run(next.signal))))];
                    case 3: return [4 /*yield*/, __await.apply(void 0, [_b.sent()])];
                    case 4: return [4 /*yield*/, __await.apply(void 0, [(_a.value = _b.sent(), _a)])];
                    case 5: return [2 /*return*/, _b.sent()];
                }
            });
        });
    });
    var flowWrites = function (id) {
        return writes.filter(function (w) { return w.key === 'flows' && w.id === id; }).map(function (w) { return w.value; });
    };
    return {
        clock: clock,
        requests: requests,
        children: children,
        session: session,
        writes: writes,
        observed: observed,
        flowWrites: flowWrites,
        flow: function (id) { return flowWrites(id).at(-1); },
        stream: function () { var _a; return (_a = writes.filter(function (w) { return w.key === 'stream'; }).at(-1)) === null || _a === void 0 ? void 0 : _a.value; },
        child: function () {
            var last = children.at(-1);
            if (last === undefined)
                throw new Error('no process started');
            return last;
        },
    };
}
/** A CloudEvents event as the service sends it. */
function update(flow, id, data) {
    return {
        specversion: '1.0',
        id: id,
        source: "nowcast://bricks/".concat(flow.split('/')[0]),
        type: 'probe.item/v1',
        subject: flow,
        time: "2026-11-06T10:00:".concat(id.padStart(2, '0'), "Z"),
        dataschema: 'nowcast://bricks/probe/types/probe.item/v1',
        data: data,
    };
}
exports.START = { cwd: '/', surface: null, isInteractive: false };
/**
 * Another mod, following the nowcast mod's state as any mod would: through a
 * `state.set` hook, and `$.state.get`. It runs apart from the test, so it says
 * what it saw with `$.ui.log`, which the world records.
 */
exports.observer = {
    name: 'observer',
    register: function (on) {
        on('state.set', { plugin: 'nowcast', key: 'flows' }, function ($, e, next) { return __awaiter(void 0, void 0, void 0, function () {
            var result;
            var _a, _b;
            return __generator(this, function (_c) {
                switch (_c.label) {
                    case 0: return [4 /*yield*/, next(e)];
                    case 1:
                        result = _c.sent();
                        $.ui.log("observer set ".concat(e.id, " ").concat(JSON.stringify((_b = (_a = e.value.last) === null || _a === void 0 ? void 0 : _a.data) !== null && _b !== void 0 ? _b : null)));
                        return [2 /*return*/, result];
                }
            });
        }); });
        on('turn.start', function ($, e, next) { return __awaiter(void 0, void 0, void 0, function () {
            var value;
            var _a, _b;
            return __generator(this, function (_c) {
                switch (_c.label) {
                    case 0: return [4 /*yield*/, $.state.get({ plugin: 'nowcast', key: 'flows', id: 'probe/x' })];
                    case 1:
                        value = (_c.sent()).value;
                        $.ui.log("observer get probe/x ".concat(JSON.stringify((_b = (_a = value === null || value === void 0 ? void 0 : value.last) === null || _a === void 0 ? void 0 : _a.data) !== null && _b !== void 0 ? _b : null)));
                        return [2 /*return*/, next(e)];
                }
            });
        }); });
    },
};
