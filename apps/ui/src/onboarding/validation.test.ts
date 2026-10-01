// The forms' checks against the contracts: setup (api-v1.md, "The first run") and a typed SSH host
// (desktop-gateway.md, "Remote workspaces").

import { describe, expect, it } from 'vitest';
import {
  checkCpus,
  checkHandle,
  checkHost,
  checkSetup,
  codePoints,
  fieldOfMessage,
  suggestHandle,
  trimmedSetup,
  type SetupValues,
} from './validation.ts';

const OK: SetupValues = { workspaceName: 'Demo Lab', personName: 'Sam Rivera', handle: '@sam', machineName: 'This laptop' };

describe('setup', () => {
  it('accepts the contract example', () => {
    expect(checkSetup(OK)).toEqual({});
  });

  it.each([
    ['workspaceName', '', 'Give the workspace a name.'],
    ['workspaceName', '   \t ', 'Give the workspace a name.'],
    ['workspaceName', 'x'.repeat(81), 'Keep the workspace name to 80 characters.'],
    ['workspaceName', 'Demo\u0007Lab', 'The workspace name cannot contain control characters.'],
    ['workspaceName', 'Demo\u0085Lab', 'The workspace name cannot contain control characters.'],
    ['personName', '', 'Enter your name.'],
    ['personName', 'y'.repeat(81), 'Keep your name to 80 characters.'],
    ['machineName', '', 'Give this machine a name.'],
    ['machineName', 'z'.repeat(61), 'Keep the machine name to 60 characters.'],
  ] as const)('refuses %s = %j', (field, value, message) => {
    expect(checkSetup({ ...OK, [field]: value })).toEqual({ [field]: message });
  });

  it('counts code points after trimming, not UTF-16 units', () => {
    // 80 astral characters are 160 UTF-16 units, and still 80 code points.
    const astral = '🚀'.repeat(80);
    expect(astral.length).toBe(160);
    expect(codePoints(astral)).toBe(80);
    expect(checkSetup({ ...OK, workspaceName: `  ${astral}  ` })).toEqual({});
    expect(checkSetup({ ...OK, machineName: ` ${'é'.repeat(60)} ` })).toEqual({});
  });

  it.each([
    ['@sam', undefined],
    ['@a', undefined],
    [`@${'a'.repeat(32)}`, undefined],
    ['@sam_rivera-2', undefined],
    ['', 'Give yourself a handle.'],
    ['sam', 'A handle is "@" and 1 to 32 lower-case letters, digits, "_" or "-".'],
    ['@', 'A handle is "@" and 1 to 32 lower-case letters, digits, "_" or "-".'],
    ['@Sam', 'A handle is "@" and 1 to 32 lower-case letters, digits, "_" or "-".'],
    ['@sam rivera', 'A handle is "@" and 1 to 32 lower-case letters, digits, "_" or "-".'],
    [' @sam', 'A handle is "@" and 1 to 32 lower-case letters, digits, "_" or "-".'],
    [`@${'a'.repeat(33)}`, 'A handle is "@" and 1 to 32 lower-case letters, digits, "_" or "-".'],
  ])('checks the handle %j', (handle, message) => {
    expect(checkHandle(handle)).toBe(message);
  });

  it('trims the names but never the handle', () => {
    expect(trimmedSetup({ workspaceName: ' Demo Lab ', personName: '\tSam ', handle: '@sam', machineName: ' laptop\n' })).toEqual({
      workspaceName: 'Demo Lab',
      personName: 'Sam',
      handle: '@sam',
      machineName: 'laptop',
    });
  });

  it.each([
    ['Sam Rivera', '@sam'],
    ['  Zoë Ödegaard ', '@zoe'],
    ['Jean-Luc', '@jean-luc'],
    ['O’Brien', '@obrien'],
    ['Ｓａｍ', '@sam'],
    ['李雷', ''],
    ['', ''],
    [`${'a'.repeat(40)} B`, `@${'a'.repeat(32)}`],
  ])('suggests a handle from %j', (name, handle) => {
    expect(suggestHandle(name)).toBe(handle);
  });

  it.each([
    ['workspace_name must be 1 to 80 characters.', 'workspaceName'],
    ['person.name must not contain control characters.', 'personName'],
    ['person.handle must be "@" followed by 1 to 32 of a-z, 0-9, "_" or "-".', 'handle'],
    ['The handle @sam is already taken.', 'handle'],
    ['machine_name must be 1 to 60 characters.', 'machineName'],
    ['The body is not JSON.', undefined],
  ])('finds the field a hub message names: %j', (message, field) => {
    expect(fieldOfMessage(message)).toBe(field);
  });
});

describe('a typed host', () => {
  it.each([
    ['hpc-login', undefined],
    ['sam@server.example.org', undefined],
    ['10.0.0.7', undefined],
    ['', 'Pick a host, or type one.'],
    ['-oProxyCommand=sh', 'A host cannot start with "-".'],
    ['-J', 'A host cannot start with "-".'],
    ['hpc login', 'A host cannot contain spaces or control characters.'],
    ['hpc\tlogin', 'A host cannot contain spaces or control characters.'],
    ['hpc\nlogin', 'A host cannot contain spaces or control characters.'],
    ['hpc\u0000', 'A host cannot contain spaces or control characters.'],
    ['h'.repeat(256), 'That host name is too long.'],
  ])('checks %j', (host, message) => {
    expect(checkHost(host)).toBe(message);
  });

  it.each([
    ['', undefined],
    ['4', undefined],
    [' 16 ', undefined],
    ['0', 'CPUs is a whole number, 1 or more.'],
    ['1.5', 'CPUs is a whole number, 1 or more.'],
    ['-2', 'CPUs is a whole number, 1 or more.'],
    ['four', 'CPUs is a whole number, 1 or more.'],
  ])('checks CPUs %j', (value, message) => {
    expect(checkCpus(value)).toBe(message);
  });
});
