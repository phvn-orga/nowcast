#!/usr/bin/env node
// baudrier agents-md-reminder, version 1.
//
// PreToolUse hook. When a tool is about to write or edit an AGENTS.md or
// CLAUDE.md, it adds a reminder to follow the project's editing-agents-md
// skill. It informs and never blocks: whatever the input, it exits 0. It
// serves the editing-agents-md module of the baudrier plugin.
//
// This is a reference script: the harness-setup skill copies it into a project
// and declares it in the project's hook settings. Keep the version line above
// so a review can tell how far a copy has drifted from the reference.

import { readFileSync, realpathSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const REMINDER =
  'This file is an agent instruction file: follow the editing-agents-md skill ' +
  '(route the change first, directives only, no history, within the size limits), then run the ' +
  "project's check-agents-md script.";

// Returns the hook's stdout for one PreToolUse input: the reminder, or nothing.
export function reminder(input) {
  const path = input?.tool_input?.file_path;
  if (typeof path !== 'string' || !/(^|[\\/])(AGENTS|CLAUDE)\.md$/.test(path)) return '';
  return JSON.stringify({ hookSpecificOutput: { hookEventName: 'PreToolUse', additionalContext: REMINDER } }) + '\n';
}

if (process.argv[1] && realpathSync(process.argv[1]) === fileURLToPath(import.meta.url)) {
  let input = null;
  try {
    input = JSON.parse(readFileSync(0, 'utf8'));
  } catch {
    // Not a hook input: nothing to remind.
  }
  process.stdout.write(reminder(input));
}
