"use strict";
// What `nowcast --json watch` prints, read without touching the engine.
Object.defineProperty(exports, "__esModule", { value: true });
exports.Lines = void 0;
exports.parseEvent = parseEvent;
exports.nextFlow = nextFlow;
exports.parseConnection = parseConnection;
/** Splits text that arrives in pieces into whole lines. */
var Lines = /** @class */ (function () {
    function Lines() {
        this.rest = '';
    }
    /** The lines `text` completes; an unfinished one waits for the next piece. */
    Lines.prototype.push = function (text) {
        var _a;
        var lines = (this.rest + text).split('\n');
        this.rest = (_a = lines.pop()) !== null && _a !== void 0 ? _a : '';
        return lines.map(function (line) { return line.replace(/\r$/, ''); }).filter(function (line) { return line !== ''; });
    };
    return Lines;
}());
exports.Lines = Lines;
/**
 * Reads one stdout line: `{"event": <name>, "data": <data>}`. Null for a line
 * that is not one of the three events of the contract, or lacks its flow.
 */
function parseEvent(line) {
    var _a;
    var parsed;
    try {
        parsed = JSON.parse(line);
    }
    catch (_b) {
        return null;
    }
    if (!isObject(parsed) || !isObject(parsed.data))
        return null;
    var data = parsed.data;
    switch (parsed.event) {
        case 'update':
            return typeof data.subject === 'string' && typeof data.id === 'string'
                ? { event: 'update', flow: data.subject, update: data }
                : null;
        case 'flow-error':
            return typeof data.flow === 'string'
                ? { event: 'flow-error', flow: data.flow, message: String((_a = data.message) !== null && _a !== void 0 ? _a : '') }
                : null;
        case 'flow-ended':
            return typeof data.flow === 'string' ? { event: 'flow-ended', flow: data.flow } : null;
        default:
            return null;
    }
}
/**
 * The flow's state once `event` is applied to `previous`, or null when it
 * changes nothing: an update seen already, which a new stream sends again.
 */
function nextFlow(previous, event) {
    var _a;
    var last = (_a = previous === null || previous === void 0 ? void 0 : previous.last) !== null && _a !== void 0 ? _a : null;
    switch (event.event) {
        case 'update':
            if (previous !== undefined &&
                !previous.ended &&
                previous.error === null &&
                (last === null || last === void 0 ? void 0 : last.id) === event.update.id &&
                last.time === event.update.time) {
                return null;
            }
            return { last: event.update, error: null, ended: false };
        case 'flow-error':
            return { last: last, error: event.message, ended: false };
        case 'flow-ended':
            return { last: last, error: null, ended: true };
    }
}
/**
 * `nowcast: connected` once the service is back, `nowcast: <reason>;
 * retrying` when it is lost. Anything else says nothing about it.
 */
function parseConnection(line) {
    var _a;
    var text = line.replace(/^nowcast: /, '');
    if (text === line)
        return null;
    if (text === 'connected')
        return { status: 'up' };
    var reason = (_a = text.match(/^(.*); retrying$/)) === null || _a === void 0 ? void 0 : _a[1];
    return reason === undefined ? null : { status: 'down', reason: reason };
}
function isObject(value) {
    return typeof value === 'object' && value !== null && !Array.isArray(value);
}
