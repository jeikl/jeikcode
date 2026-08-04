import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  collapseHomePath,
  displayPath,
  pathBasename,
  stripExtendedPathPrefix,
} from './displayPath.ts';

test('strips Windows extended path prefix', () => {
  assert.equal(stripExtendedPathPrefix('\\\\?\\E:\\desktop'), 'E:\\desktop');
  assert.equal(stripExtendedPathPrefix('//?/E:/desktop'), 'E:/desktop');
  assert.equal(
    stripExtendedPathPrefix('\\\\?\\UNC\\server\\share\\x'),
    '\\\\server\\share\\x',
  );
  assert.equal(stripExtendedPathPrefix('/home/u/proj'), '/home/u/proj');
});

test('displayPath collapses home and strips extended prefix', () => {
  assert.equal(displayPath('\\\\?\\E:\\desktop'), 'E:\\desktop');
  assert.equal(displayPath('/Users/me/code/agents'), '~/code/agents');
});

test('pathBasename is separator-agnostic', () => {
  assert.equal(pathBasename('\\\\?\\E:\\desktop'), 'desktop');
  assert.equal(pathBasename('E:\\foo\\bar'), 'bar');
  assert.equal(pathBasename('/home/u/proj'), 'proj');
});

test('collapseHomePath handles Windows Users', () => {
  assert.equal(collapseHomePath('C:\\Users\\me\\proj'), '~\\proj');
});
