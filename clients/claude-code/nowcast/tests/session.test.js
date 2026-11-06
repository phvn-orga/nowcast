"use strict";
// The session as the service sees it: declared first, declared again on
// each change, one stream per session (contract section 3, "Session").
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
Object.defineProperty(exports, "__esModule", { value: true });
var testing_1 = require("claude-code/testing");
var world_1 = require("./world");
var RESUME = { id: 'session-1' };
(0, testing_1.test)('the session is declared before the stream opens', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w;
    return __generator(this, function (_a) {
        switch (_a.label) {
            case 0:
                w = (0, world_1.world)(on);
                return [4 /*yield*/, $.session.start(world_1.START)];
            case 1:
                _a.sent();
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _a.sent();
                (0, testing_1.expect)(w.requests).toEqual([
                    { method: 'PUT', url: 'http://nowcast/v1/session', body: '{"id":"session-1"}', socketPath: world_1.SOCKET },
                ]);
                (0, testing_1.expect)(w.children.map(function (c) { return c.argv; })).toEqual([['nowcast', '--json', '--session', 'session-1', 'watch']]);
                (0, testing_1.expect)(w.stream()).toEqual({ session: 'session-1', status: 'up', reconnections: 0, error: null });
                return [2 /*return*/];
        }
    });
}); });
(0, testing_1.test)('NOWCAST_SOCKET names the socket, as for the command', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w;
    return __generator(this, function (_a) {
        switch (_a.label) {
            case 0:
                w = (0, world_1.world)(on, { env: { NOWCAST_SOCKET: '/tmp/x/nowcast.sock', XDG_RUNTIME_DIR: '/run/user/1000' } });
                return [4 /*yield*/, $.session.start(world_1.START)];
            case 1:
                _a.sent();
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _a.sent();
                (0, testing_1.expect)(w.requests.map(function (r) { return r.socketPath; })).toEqual(['/tmp/x/nowcast.sock']);
                return [2 /*return*/];
        }
    });
}); });
(0, testing_1.test)('without a socket path, the stream still starts and the state says why', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w;
    return __generator(this, function (_a) {
        switch (_a.label) {
            case 0:
                w = (0, world_1.world)(on, { env: { XDG_RUNTIME_DIR: 'relative' } });
                return [4 /*yield*/, $.session.start(world_1.START)];
            case 1:
                _a.sent();
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _a.sent();
                (0, testing_1.expect)(w.requests).toEqual([]);
                (0, testing_1.expect)(w.children.length).toBe(1);
                (0, testing_1.expect)(w.stream()).toEqual({
                    session: 'session-1',
                    status: 'starting',
                    reconnections: 0,
                    error: 'neither NOWCAST_SOCKET nor an absolute XDG_RUNTIME_DIR is set',
                });
                return [2 /*return*/];
        }
    });
}); });
(0, testing_1.test)('a refused declaration is reported', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w;
    var _a;
    return __generator(this, function (_b) {
        switch (_b.label) {
            case 0:
                w = (0, world_1.world)(on, { answer: function () { return ({ status: 400, text: '{"error":{}}' }); } });
                return [4 /*yield*/, $.session.start(world_1.START)];
            case 1:
                _b.sent();
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _b.sent();
                (0, testing_1.expect)((_a = w.stream()) === null || _a === void 0 ? void 0 : _a.error).toBe('PUT /v1/session answered 400: {"error":{}}');
                return [2 /*return*/];
        }
    });
}); });
var _loop_1 = function (reason) {
    (0, testing_1.test)("after a ".concat(reason, ", the new session is declared and the stream keeps delivering"), function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
        var w, first, second;
        var _a;
        return __generator(this, function (_b) {
            switch (_b.label) {
                case 0:
                    w = (0, world_1.world)(on);
                    return [4 /*yield*/, $.session.start(world_1.START)];
                case 1:
                    _b.sent();
                    return [4 /*yield*/, w.clock.settle()];
                case 2:
                    _b.sent();
                    first = w.child();
                    first.event('update', (0, world_1.update)('probe/x', '1', { n: 1 }));
                    return [4 /*yield*/, w.clock.settle()];
                case 3:
                    _b.sent();
                    return [4 /*yield*/, $.session.end({ reason: reason, sessionId: 'session-1', resume: RESUME })];
                case 4:
                    _b.sent();
                    w.session.id = 'session-2';
                    return [4 /*yield*/, w.clock.advance(50)];
                case 5:
                    _b.sent();
                    (0, testing_1.expect)(w.requests.map(function (r) { return r.body; })).toEqual(['{"id":"session-1"}', '{"id":"session-2"}']);
                    (0, testing_1.expect)(first.killed).toBe(true);
                    second = w.child();
                    (0, testing_1.expect)(second.session).toBe('session-2');
                    (0, testing_1.expect)((_a = w.stream()) === null || _a === void 0 ? void 0 : _a.session).toBe('session-2');
                    // The new stream sends the last value again: the new session's state,
                    // which starts empty, gets it.
                    second.event('update', (0, world_1.update)('probe/x', '1', { n: 1 }));
                    second.event('update', (0, world_1.update)('probe/x', '2', { n: 2 }));
                    return [4 /*yield*/, w.clock.settle()];
                case 6:
                    _b.sent();
                    (0, testing_1.expect)(w.flowWrites('probe/x').map(function (f) { var _a; return (_a = f.last) === null || _a === void 0 ? void 0 : _a.data; })).toEqual([{ n: 1 }, { n: 1 }, { n: 2 }]);
                    // Subscriptions follow the process, not the session: nothing subscribes again.
                    (0, testing_1.expect)(w.requests.every(function (r) { return r.url === 'http://nowcast/v1/session'; })).toBe(true);
                    return [2 /*return*/];
            }
        });
    }); });
};
for (var _i = 0, _a = ['clear', 'resume']; _i < _a.length; _i++) {
    var reason = _a[_i];
    _loop_1(reason);
}
(0, testing_1.test)('the new id is awaited until it shows', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w;
    return __generator(this, function (_a) {
        switch (_a.label) {
            case 0:
                w = (0, world_1.world)(on);
                return [4 /*yield*/, $.session.start(world_1.START)];
            case 1:
                _a.sent();
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _a.sent();
                return [4 /*yield*/, $.session.end({ reason: 'clear', sessionId: 'session-1', resume: RESUME })];
            case 3:
                _a.sent();
                return [4 /*yield*/, w.clock.advance(500)];
            case 4:
                _a.sent();
                (0, testing_1.expect)(w.children.length).toBe(1);
                w.session.id = 'session-2';
                return [4 /*yield*/, w.clock.advance(50)];
            case 5:
                _a.sent();
                (0, testing_1.expect)(w.children.map(function (c) { return c.session; })).toEqual(['session-1', 'session-2']);
                return [2 /*return*/];
        }
    });
}); });
(0, testing_1.test)('an exit ends nothing early: the stream stays until the process goes', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w;
    return __generator(this, function (_a) {
        switch (_a.label) {
            case 0:
                w = (0, world_1.world)(on);
                return [4 /*yield*/, $.session.start(world_1.START)];
            case 1:
                _a.sent();
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _a.sent();
                return [4 /*yield*/, $.session.end({ reason: 'prompt_input_exit', sessionId: 'session-1', resume: RESUME })];
            case 3:
                _a.sent();
                return [4 /*yield*/, w.clock.advance(10000)];
            case 4:
                _a.sent();
                (0, testing_1.expect)(w.children.length).toBe(1);
                (0, testing_1.expect)(w.child().running).toBe(true);
                (0, testing_1.expect)(w.requests.length).toBe(1);
                return [2 /*return*/];
        }
    });
}); });
(0, testing_1.test)('a turn under another session id declares it', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w;
    var _a;
    return __generator(this, function (_b) {
        switch (_b.label) {
            case 0:
                w = (0, world_1.world)(on);
                return [4 /*yield*/, $.session.start(world_1.START)];
            case 1:
                _b.sent();
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _b.sent();
                return [4 /*yield*/, $.turn.start({ text: 'hi', turnId: 't-1' })];
            case 3:
                _b.sent();
                return [4 /*yield*/, w.clock.settle()];
            case 4:
                _b.sent();
                (0, testing_1.expect)(w.children.length).toBe(1);
                w.session.id = 'session-2';
                return [4 /*yield*/, $.turn.start({ text: 'hi', turnId: 't-2' })];
            case 5:
                _b.sent();
                return [4 /*yield*/, w.clock.settle()];
            case 6:
                _b.sent();
                (0, testing_1.expect)(w.children.map(function (c) { return c.session; })).toEqual(['session-1', 'session-2']);
                (0, testing_1.expect)((_a = w.children[0]) === null || _a === void 0 ? void 0 : _a.killed).toBe(true);
                return [2 /*return*/];
        }
    });
}); });
(0, testing_1.test)('a second start under the same session changes nothing', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w;
    return __generator(this, function (_a) {
        switch (_a.label) {
            case 0:
                w = (0, world_1.world)(on);
                return [4 /*yield*/, $.session.start(world_1.START)];
            case 1:
                _a.sent();
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _a.sent();
                return [4 /*yield*/, $.session.start(world_1.START)];
            case 3:
                _a.sent();
                return [4 /*yield*/, w.clock.settle()];
            case 4:
                _a.sent();
                (0, testing_1.expect)(w.children.length).toBe(1);
                (0, testing_1.expect)(w.requests.length).toBe(1);
                return [2 /*return*/];
        }
    });
}); });
