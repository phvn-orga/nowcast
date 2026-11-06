"use strict";
// The session's stream redistributed through the state: the three events of
// contract section 3 ("Le flux d'événements"), as `nowcast watch --json`
// prints them.
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
function started($, on) {
    return __awaiter(this, void 0, void 0, function () {
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
                    return [2 /*return*/, w];
            }
        });
    });
}
(0, testing_1.test)('each update is written under its flow id', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w;
    var _a;
    return __generator(this, function (_b) {
        switch (_b.label) {
            case 0: return [4 /*yield*/, started($, on)];
            case 1:
                w = _b.sent();
                w.child().event('update', (0, world_1.update)('probe/x', '1', { n: 1 }));
                w.child().event('update', (0, world_1.update)('probe/a/b#c d', '2', { n: 2 }));
                w.child().event('update', (0, world_1.update)('probe/x', '3', { n: 3 }));
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _b.sent();
                (0, testing_1.expect)(w.flowWrites('probe/x')).toEqual([
                    { last: (0, world_1.update)('probe/x', '1', { n: 1 }), error: null, ended: false },
                    { last: (0, world_1.update)('probe/x', '3', { n: 3 }), error: null, ended: false },
                ]);
                (0, testing_1.expect)((_a = w.flow('probe/a/b#c d')) === null || _a === void 0 ? void 0 : _a.last).toEqual((0, world_1.update)('probe/a/b#c d', '2', { n: 2 }));
                return [2 /*return*/];
        }
    });
}); });
(0, testing_1.test)('other mods follow the flows and read them', { plugins: [world_1.observer] }, function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w;
    return __generator(this, function (_a) {
        switch (_a.label) {
            case 0: return [4 /*yield*/, started($, on)];
            case 1:
                w = _a.sent();
                w.child().event('update', (0, world_1.update)('probe/x', '1', { n: 1 }));
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _a.sent();
                return [4 /*yield*/, $.turn.start({ text: '', turnId: 't-1' })];
            case 3:
                _a.sent();
                (0, testing_1.expect)(w.observed).toEqual(['set probe/x {"n":1}', 'get probe/x {"n":1}']);
                return [2 /*return*/];
        }
    });
}); });
(0, testing_1.test)('a value seen already is not written again', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w;
    return __generator(this, function (_a) {
        switch (_a.label) {
            case 0: return [4 /*yield*/, started($, on)];
            case 1:
                w = _a.sent();
                w.child().event('update', (0, world_1.update)('probe/x', '1', { n: 1 }));
                w.child().event('update', (0, world_1.update)('probe/x', '1', { n: 1 }));
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _a.sent();
                (0, testing_1.expect)(w.flowWrites('probe/x').length).toBe(1);
                return [2 /*return*/];
        }
    });
}); });
(0, testing_1.test)('a flow error keeps the last value until the next update', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w;
    return __generator(this, function (_a) {
        switch (_a.label) {
            case 0: return [4 /*yield*/, started($, on)];
            case 1:
                w = _a.sent();
                w.child().event('update', (0, world_1.update)('probe/x', '1', { n: 1 }));
                w.child().event('flow-error', { flow: 'probe/x', message: 'source down' });
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _a.sent();
                (0, testing_1.expect)(w.flow('probe/x')).toEqual({ last: (0, world_1.update)('probe/x', '1', { n: 1 }), error: 'source down', ended: false });
                w.child().event('update', (0, world_1.update)('probe/x', '1', { n: 1 }));
                return [4 /*yield*/, w.clock.settle()];
            case 3:
                _a.sent();
                (0, testing_1.expect)(w.flow('probe/x')).toEqual({ last: (0, world_1.update)('probe/x', '1', { n: 1 }), error: null, ended: false });
                return [2 /*return*/];
        }
    });
}); });
(0, testing_1.test)('an error on a flow with no value yet', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w;
    return __generator(this, function (_a) {
        switch (_a.label) {
            case 0: return [4 /*yield*/, started($, on)];
            case 1:
                w = _a.sent();
                w.child().event('flow-error', { flow: 'probe/y', message: 'no such item' });
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _a.sent();
                (0, testing_1.expect)(w.flow('probe/y')).toEqual({ last: null, error: 'no such item', ended: false });
                return [2 /*return*/];
        }
    });
}); });
(0, testing_1.test)('an ended flow says so, and comes back with a later update', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w;
    var _a;
    return __generator(this, function (_b) {
        switch (_b.label) {
            case 0: return [4 /*yield*/, started($, on)];
            case 1:
                w = _b.sent();
                w.child().event('update', (0, world_1.update)('probe/x', '1', { n: 1 }));
                w.child().event('flow-ended', { flow: 'probe/x' });
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _b.sent();
                (0, testing_1.expect)(w.flow('probe/x')).toEqual({ last: (0, world_1.update)('probe/x', '1', { n: 1 }), error: null, ended: true });
                w.child().event('update', (0, world_1.update)('probe/x', '1', { n: 1 }));
                return [4 /*yield*/, w.clock.settle()];
            case 3:
                _b.sent();
                (0, testing_1.expect)((_a = w.flow('probe/x')) === null || _a === void 0 ? void 0 : _a.ended).toBe(false);
                return [2 /*return*/];
        }
    });
}); });
(0, testing_1.test)('an event split across pieces is read whole', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w, line;
    var _a, _b;
    return __generator(this, function (_c) {
        switch (_c.label) {
            case 0: return [4 /*yield*/, started($, on)];
            case 1:
                w = _c.sent();
                line = "".concat(JSON.stringify({ event: 'update', data: (0, world_1.update)('probe/x', '1', { text: 'é'.repeat(50) }) }), "\n");
                w.child().stdout(line.slice(0, 40));
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _c.sent();
                (0, testing_1.expect)(w.flowWrites('probe/x')).toEqual([]);
                w.child().stdout(line.slice(40));
                return [4 /*yield*/, w.clock.settle()];
            case 3:
                _c.sent();
                (0, testing_1.expect)((_b = (_a = w.flow('probe/x')) === null || _a === void 0 ? void 0 : _a.last) === null || _b === void 0 ? void 0 : _b.data).toEqual({ text: 'é'.repeat(50) });
                return [2 /*return*/];
        }
    });
}); });
(0, testing_1.test)('lines outside the contract are skipped', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w;
    return __generator(this, function (_a) {
        switch (_a.label) {
            case 0: return [4 /*yield*/, started($, on)];
            case 1:
                w = _a.sent();
                w.child().stdout('not json\n');
                w.child().event('heartbeat', {});
                w.child().event('update', { id: '1' });
                w.child().event('flow-ended', {});
                w.child().stdout('[1,2]\n\n');
                w.child().event('update', (0, world_1.update)('probe/x', '1', { n: 1 }));
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _a.sent();
                (0, testing_1.expect)(w.writes.filter(function (write) { return write.key === 'flows'; }).map(function (write) { return write.id; })).toEqual(['probe/x']);
                (0, testing_1.expect)(w.child().running).toBe(true);
                return [2 /*return*/];
        }
    });
}); });
(0, testing_1.test)('a write refused by another mod is skipped, and the stream goes on', function ($, on) { return __awaiter(void 0, void 0, void 0, function () {
    var w;
    var _a, _b;
    return __generator(this, function (_c) {
        switch (_c.label) {
            case 0:
                w = (0, world_1.world)(on, { refuse: 'probe/x' });
                return [4 /*yield*/, $.session.start(world_1.START)];
            case 1:
                _c.sent();
                return [4 /*yield*/, w.clock.settle()];
            case 2:
                _c.sent();
                w.child().event('update', (0, world_1.update)('probe/x', '1', { n: 1 }));
                w.child().event('update', (0, world_1.update)('probe/y', '2', { n: 2 }));
                return [4 /*yield*/, w.clock.settle()];
            case 3:
                _c.sent();
                (0, testing_1.expect)((_b = (_a = w.flow('probe/y')) === null || _a === void 0 ? void 0 : _a.last) === null || _b === void 0 ? void 0 : _b.data).toEqual({ n: 2 });
                (0, testing_1.expect)(w.children.length).toBe(1);
                (0, testing_1.expect)(w.child().running).toBe(true);
                return [2 /*return*/];
        }
    });
}); });
