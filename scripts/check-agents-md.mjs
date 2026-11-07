#!/usr/bin/env node
// baudrier check-agents-md, version 2.
//
// Checks every AGENTS.md and CLAUDE.md of the repository (tracked, or untracked
// and not ignored): a CLAUDE.md and an AGENTS.md side by side are one file, the
// root file stays within its line budget, no line carries history, every cited
// path exists in the repository (tracked, or untracked and not ignored, so the
// result is the same in a fresh clone). With --base, what the changes since the base add stays within
// the amendment limits. It serves the editing-agents-md module of the baudrier
// plugin.
//
// This is a reference script: the harness-setup skill copies it into a project
// and adapts the parts marked ADAPT. Keep the version line above so a review
// can tell how far a copy has drifted from the reference.
//
// Usage:
//   node check-agents-md.mjs [--base <ref>]

import { execFileSync } from 'node:child_process';
import { existsSync, lstatSync, readFileSync, realpathSync } from 'node:fs';
import { join, posix } from 'node:path';
import { fileURLToPath } from 'node:url';

// ADAPT: the limits. A rule is a list item, a paragraph, a heading or a table
// row; a block is a contiguous run of changed lines. A file whose base lines
// were more than REWRITE_RATIO removed or replaced is a rewrite, judged on the
// root line budget only.
const ROOT_MAX_LINES = 150;
const RULE_MAX_CHARS = 300;
const BLOCK_MAX_CHARS = 1500;
const REWRITE_RATIO = 0.5;

// ADAPT: what reads as history. Matched on each line outside code, inline code
// removed. Keep the list short: every false positive teaches to ignore the check.
const HISTORY = [
  /\b\d{4}-\d{2}-\d{2}\b/,
  /\bsync impact report\b/i,
  /^#+\s.*\bchangelog\b/i,
  /\b(we|was|were) (added|removed|changed|moved|renamed|replaced)\b/i,
  /\bpreviously\b/i,
  /\bas of\b/i,
];

// ADAPT: which inline-code spans are checked as repository paths: those with
// one of these extensions, and those whose first segment exists.
const EXTENSIONS = /\.(md|mdx|txt|json|jsonc|ya?ml|toml|lock|[cm]?[jt]sx?|py|rb|go|rs|sh|sql)$/i;

function couldBePath(span) {
  // Not a command, URL, glob or placeholder; not a home, absolute, package or flag.
  return !/\s|:\/\/|[*?[\]{}<>$]/.test(span) && !/^[~/@-]/.test(span);
}

const NAMES = ['AGENTS.md', 'CLAUDE.md'];
const USAGE = 'usage: check-agents-md.mjs [--base <ref>]';
const SKILL = "Follow the project's editing-agents-md skill to fix them.";

class Failure extends Error {
  constructor(code, message) {
    super(message);
    this.code = code;
  }
}

function git(cwd, ...args) {
  try {
    return execFileSync('git', args, { cwd, encoding: 'utf8', stdio: 'pipe' });
  } catch {
    throw new Failure(2, `git ${args.join(' ')} failed in ${cwd}: run inside a git repository.`);
  }
}

function gitOrNull(cwd, ...args) {
  try {
    return execFileSync('git', args, { cwd, encoding: 'utf8', stdio: 'pipe' });
  } catch {
    return null;
  }
}

function parseArgs(args) {
  if (!args.length) return null;
  if (args.length === 2 && args[0] === '--base' && args[1]) return args[1];
  throw new Failure(2, USAGE);
}

function splitLines(text) {
  const lines = text.split('\n').map((line) => line.replace(/\r$/, ''));
  if (lines.at(-1) === '') lines.pop();
  return lines;
}

// The repository's files on disk (tracked, or untracked and not ignored), and
// every directory that holds one: what a fresh clone has.
function repositoryPaths(root) {
  const listed = git(root, 'ls-files', '--cached', '--others', '--exclude-standard');
  const files = [...new Set(splitLines(listed))].filter((file) => existsSync(join(root, file)));
  const paths = new Set(['.']);
  for (const file of files) {
    for (let path = file; path !== '.'; path = posix.dirname(path)) paths.add(path);
  }
  return { files, paths };
}

// Instruction files, real files before symlinks so that a symlinked pair is
// read once, under the name of the real file.
function instructionFiles(root, listed) {
  const files = listed.filter((file) => NAMES.includes(posix.basename(file))).sort();
  const symlink = (file) => lstatSync(join(root, file)).isSymbolicLink();
  return [...files.filter((f) => !symlink(f)), ...files.filter(symlink)];
}

function pairViolations(root, files) {
  const errors = [];
  for (const claude of files.filter((f) => posix.basename(f) === 'CLAUDE.md')) {
    const agents = posix.join(posix.dirname(claude), 'AGENTS.md');
    if (!files.includes(agents)) continue;
    const same = realpathSync(join(root, claude)) === realpathSync(join(root, agents));
    if (same || importsOnly(root, claude, 'AGENTS.md') || importsOnly(root, agents, 'CLAUDE.md')) continue;
    errors.push(
      `${claude}: pair: ${claude} and ${agents} are two different files. ` +
        'Make one a symlink to the other, or reduce one to an import of the other ("@AGENTS.md").',
    );
  }
  return errors;
}

function importsOnly(root, file, other) {
  const content = splitLines(readFileSync(join(root, file), 'utf8'))
    .map((line) => line.trim())
    .filter(Boolean);
  return content.length === 1 && (content[0] === `@${other}` || content[0] === `@./${other}`);
}

// Lines outside fenced code blocks, with their 1-based number.
function proseLines(lines) {
  const result = [];
  let fenced = false;
  lines.forEach((line, i) => {
    if (/^\s*(```|~~~)/.test(line)) fenced = !fenced;
    else if (!fenced) result.push({ number: i + 1, text: line });
  });
  return result;
}

// Where a cited path resolves: from the file's directory, then from the root.
function candidates(file, path) {
  return [posix.join(posix.dirname(file), path), posix.normalize(path)].map((p) => p.replace(/\/$/, '') || '.');
}

function lineViolations(root, paths, file, lines) {
  const exists = (path) => candidates(file, path).some((p) => paths.has(p));
  const errors = [];
  for (const { number, text } of proseLines(lines)) {
    const where = `${file}:${number}`;
    const prose = text.replace(/`[^`]*`/g, '');
    for (const pattern of HISTORY) {
      const match = prose.match(pattern);
      if (match) {
        errors.push(
          `${where}: history: "${match[0]}" is history. ` +
            'Keep it where the project keeps history, and point there if needed.',
        );
      }
    }
    const spans = [...text.matchAll(/`([^`]+)`/g)].map((m) => m[1]).filter(couldBePath);
    const imported = text.trim().match(/^@([^\s~/]\S*)$/);
    if (imported) spans.push(imported[1]);
    for (const span of spans) {
      const path = span.replace(/:\d+(:\d+)?$/, '');
      const first = path.replace(/^\.\//, '').split('/')[0];
      const checked = imported?.[1] === span || EXTENSIONS.test(path) || exists(first);
      if (!checked || exists(path)) continue;
      if (candidates(file, path).some((p) => !p.startsWith('..') && existsSync(join(root, p)))) {
        errors.push(
          `${where}: reference: \`${span}\` is ignored by git, so a fresh clone and CI do not have it. ` +
            'Cite a file of the repository, or remove it.',
        );
      } else {
        errors.push(`${where}: reference: \`${span}\` does not exist. Fix the path or remove it.`);
      }
    }
  }
  return errors;
}

// A rule starts at a list item, a heading or a table row, or after a blank line.
function rules(lines) {
  const result = [];
  let current = null;
  for (const { number, text } of lines) {
    const trimmed = text.trim();
    if (!trimmed) {
      current = null;
      continue;
    }
    const starts = /^([-*+]|\d+[.)])\s|^#{1,6}\s|^\|/.test(trimmed);
    if (!current || starts) {
      current = { number, text: trimmed };
      result.push(current);
      if (/^(#|\|)/.test(trimmed)) current = null;
    } else {
      current.text += ` ${trimmed}`;
    }
  }
  return result;
}

function hunks(diff) {
  const result = [];
  let hunk = null;
  for (const line of splitLines(diff)) {
    const header = line.match(/^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@/);
    if (header) {
      hunk = { start: Number(header[1]), added: [], removed: [] };
      result.push(hunk);
    } else if (hunk && line.startsWith('+')) {
      hunk.added.push({ number: hunk.start + hunk.added.length, text: line.slice(1) });
    } else if (hunk && line.startsWith('-')) {
      hunk.removed.push({ number: 0, text: line.slice(1) });
    }
  }
  return result;
}

function amendmentViolations(root, file, base) {
  const before = gitOrNull(root, 'show', `${base}:${file}`);
  if (before === null) return [];
  const changes = hunks(git(root, 'diff', '--unified=0', '--no-color', '--no-ext-diff', base, '--', file));
  const removed = changes.reduce((sum, h) => sum + h.removed.length, 0);
  const baseLines = splitLines(before).length;
  if (!baseLines || removed / baseLines > REWRITE_RATIO) return [];

  const errors = [];
  for (const hunk of changes) {
    const added = rules(hunk.added);
    const gone = rules(hunk.removed);
    let growth = 0;
    added.forEach((rule, i) => {
      const adds = rule.text.length - (gone[i]?.text.length ?? 0);
      growth += adds;
      if (adds > RULE_MAX_CHARS) {
        errors.push(
          `${file}:${rule.number}: amendment: this rule adds ${adds} characters (limit ${RULE_MAX_CHARS}). ` +
            'Shorten it, or move the detail where it belongs and point to it.',
        );
      }
    });
    growth -= gone.slice(added.length).reduce((sum, rule) => sum + rule.text.length, 0);
    if (growth > BLOCK_MAX_CHARS) {
      errors.push(
        `${file}:${hunk.start}: amendment: this block adds ${growth} characters (limit ${BLOCK_MAX_CHARS}). ` +
          'Move what only some tasks need into a nested file, a skill or a doc, and point to it.',
      );
    }
  }
  return errors;
}

function mergeBase(root, ref) {
  const base = gitOrNull(root, 'merge-base', ref, 'HEAD');
  if (base === null) {
    throw new Failure(
      2,
      `unknown base ${ref}, or no common history with HEAD. ` +
        'Fetch it first (in CI, check out with the full history).',
    );
  }
  return base.trim();
}

function check(cwd, args) {
  const ref = parseArgs(args);
  const root = git(cwd, 'rev-parse', '--show-toplevel').trim();
  const base = ref && mergeBase(root, ref);
  const { files: listed, paths } = repositoryPaths(root);
  const files = instructionFiles(root, listed);
  if (!files.length) return 'check-agents-md: no AGENTS.md or CLAUDE.md found.\n';

  const errors = pairViolations(root, files);
  const seen = new Set();
  for (const file of files) {
    const real = realpathSync(join(root, file));
    if (seen.has(real)) continue;
    seen.add(real);
    const lines = splitLines(readFileSync(real, 'utf8'));
    if (!file.includes('/') && lines.length > ROOT_MAX_LINES) {
      errors.push(
        `${file}: size: ${lines.length} lines (limit ${ROOT_MAX_LINES}). ` +
          'Move what only some tasks need into nested files, skills or docs, and point to them.',
      );
    }
    errors.push(...lineViolations(root, paths, file, lines));
    if (base) errors.push(...amendmentViolations(root, file, base));
  }
  if (errors.length) throw new Failure(1, `${errors.join('\n')}\n${SKILL}`);
  const count = seen.size === 1 ? '1 file' : `${seen.size} files`;
  return `check-agents-md: ${count} checked, no violation.\n`;
}

// Runs the check and returns its exit code and output, without exiting.
export function main(args, cwd = process.cwd()) {
  try {
    return { code: 0, out: check(cwd, args), err: '' };
  } catch (error) {
    if (!(error instanceof Failure)) throw error;
    const prefix = error.code === 1 ? '' : 'check-agents-md: ';
    return { code: error.code, out: '', err: `${prefix}${error.message}\n` };
  }
}

if (process.argv[1] && realpathSync(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const { code, out, err } = main(process.argv.slice(2));
  process.stdout.write(out);
  process.stderr.write(err);
  process.exitCode = code;
}
