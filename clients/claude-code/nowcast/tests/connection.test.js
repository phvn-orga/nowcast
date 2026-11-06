"use strict";
// The service going and coming back. `nowcast watch` reconnects by itself,
// resuming with `Last-Event-ID`; the mod reports it, and starts the command
// again only when it exits.
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
(0, testing_1.test)('a lost service shows as down, then up once the command is back', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
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
                w.child().stderr('nowcast: cannot reach the service at /x: connection refused; retrying\n');
                return [4 /*yield*/, w.clock.settle()];
            case 3:
                _a.sent();
                (0, testing_1.expect)(w.stream()).toEqual({
                    session: 'session-1',
                    status: 'down',
                    reconnections: 0,
                    error: 'cannot reach the service at /x: connection refused',
                });
                w.child().stderr('nowcast: connected\n');
                return [4 /*yield*/, w.clock.settle()];
            case 4:
                _a.sent();
                (0, testing_1.expect)(w.stream()).toEqual({ session: 'session-1', status: 'up', reconnections: 1, error: null });
                // The command reconnected itself: the same process, no new declaration.
                (0, testing_1.expect)(w.children.length).toBe(1);
                (0, testing_1.expect)(w.requests.length).toBe(1);
                return [2 /*return*/];
        }
    });
}); });
(0, testing_1.test)('a service absent at start is waited for', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w;
    var _a, _b;
    return __generator(this, function (_c) {
        switch (_c.label) {
            case 0:
                w = (0, world_1.world)(on, { answer: function () { return 'connect ENOENT'; } });
                return [4 /*yield*/, $.session.start(world_1.START)];
            case 1:
                _c.sent();
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _c.sent();
                (0, testing_1.expect)((_a = w.stream()) === null || _a === void 0 ? void 0 : _a.status).toBe('starting');
                (0, testing_1.expect)((_b = w.stream()) === null || _b === void 0 ? void 0 : _b.error).toBe('cannot reach the service at /run/user/1000/nowcast/nowcast.sock: nowcast: $.http.fetch: connect ENOENT');
                (0, testing_1.expect)(w.child().running).toBe(true);
                w.child().stderr('nowcast: cannot reach the service: no such file; retrying\nnowcast: connected\n');
                return [4 /*yield*/, w.clock.settle()];
            case 3:
                _c.sent();
                (0, testing_1.expect)(w.stream()).toEqual({ session: 'session-1', status: 'up', reconnections: 1, error: null });
                return [2 /*return*/];
        }
    });
}); });
(0, testing_1.test)('a command that exits is started again, later each time', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w;
    var _a, _b;
    return __generator(this, function (_c) {
        switch (_c.label) {
            case 0:
                w = (0, world_1.world)(on);
                return [4 /*yield*/, $.session.start(world_1.START)];
            case 1:
                _c.sent();
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _c.sent();
                w.child().stderr('{"error":{"code":"unexpected_response","message":"unexpected answer"}}\n');
                w.child().exit(4);
                return [4 /*yield*/, w.clock.settle()];
            case 3:
                _c.sent();
                (0, testing_1.expect)((_a = w.stream()) === null || _a === void 0 ? void 0 : _a.status).toBe('stopped');
                (0, testing_1.expect)((_b = w.stream()) === null || _b === void 0 ? void 0 : _b.error).toBe('nowcast watch exited (status 4): unexpected answer');
                return [4 /*yield*/, w.clock.advance(999)];
            case 4:
                _c.sent();
                (0, testing_1.expect)(w.children.length).toBe(1);
                return [4 /*yield*/, w.clock.advance(1)];
            case 5:
                _c.sent();
                (0, testing_1.expect)(w.children.length).toBe(2);
                (0, testing_1.expect)(w.child().session).toBe('session-1');
                w.child().exit(1);
                return [4 /*yield*/, w.clock.advance(1999)];
            case 6:
                _c.sent();
                (0, testing_1.expect)(w.children.length).toBe(2);
                return [4 /*yield*/, w.clock.advance(1)];
            case 7:
                _c.sent();
                (0, testing_1.expect)(w.children.length).toBe(3);
                return [2 /*return*/];
        }
    });
}); });
(0, testing_1.test)('a command that ran a while is started again at once', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
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
                w.child().exit(1);
                return [4 /*yield*/, w.clock.advance(1000)];
            case 3:
                _a.sent();
                return [4 /*yield*/, w.clock.advance(60000)];
            case 4:
                _a.sent();
                w.child().exit(1);
                return [4 /*yield*/, w.clock.advance(1000)];
            case 5:
                _a.sent();
                (0, testing_1.expect)(w.children.length).toBe(3);
                return [2 /*return*/];
        }
    });
}); });
(0, testing_1.test)('a command not installed is reported and tried again', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w, writes;
    var _a, _b;
    return __generator(this, function (_c) {
        switch (_c.label) {
            case 0:
                w = (0, world_1.world)(on, { spawnFails: true });
                return [4 /*yield*/, $.session.start(world_1.START)];
            case 1:
                _c.sent();
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _c.sent();
                (0, testing_1.expect)((_a = w.stream()) === null || _a === void 0 ? void 0 : _a.status).toBe('stopped');
                (0, testing_1.expect)((_b = w.stream()) === null || _b === void 0 ? void 0 : _b.error).toBe('cannot run nowcast: nowcast: $.process.spawn: nowcast: not found');
                writes = w.writes.length;
                return [4 /*yield*/, w.clock.advance(1000)];
            case 3:
                _c.sent();
                (0, testing_1.expect)(w.writes.length).toBeGreaterThan(writes);
                return [2 /*return*/];
        }
    });
}); });
(0, testing_1.test)('a command the mod stopped is not started again', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
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
                w.session.id = 'session-2';
                return [4 /*yield*/, $.turn.start({ text: '', turnId: 't-1' })];
            case 3:
                _a.sent();
                return [4 /*yield*/, w.clock.advance(120000)];
            case 4:
                _a.sent();
                (0, testing_1.expect)(w.children.map(function (c) { return [c.session, c.running]; })).toEqual([
                    ['session-1', false],
                    ['session-2', true],
                ]);
                return [2 /*return*/];
        }
    });
}); });
