import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { loadSkillInventory } from '../setup/inventory.mjs';

test('inventory derives all shipped skill names from plugin metadata and frontmatter', async () => {
  const inventory = await loadSkillInventory();

  assert.equal(inventory.skills.length, 21);
  assert.equal(new Set(inventory.skills.map((skill) => skill.frontmatterName)).size, 21);
  assert.ok(inventory.skills.some((skill) => skill.frontmatterName === 'loam::using'));
  assert.ok(inventory.skills.every((skill) => skill.aliases.includes(skill.frontmatterName)));
  assert.ok(inventory.skills.every((skill) => skill.aliases.includes(skill.directoryName)));
});

test('inventory rejects duplicate frontmatter names before setup can mutate', async () => {
  const packageRoot = await mkdtemp(join(tmpdir(), 'loam-inventory-'));
  await mkdir(join(packageRoot, '.claude-plugin'));
  await mkdir(join(packageRoot, 'skills', 'one'), { recursive: true });
  await mkdir(join(packageRoot, 'skills', 'two'), { recursive: true });
  await writeFile(
    join(packageRoot, '.claude-plugin', 'plugin.json'),
    JSON.stringify({ name: 'loam', skills: ['./skills/one', './skills/two'] }),
  );
  const skill = `---\nname: loam::duplicate\nmetadata:\n  version: "1.0.0"\n---\n`;
  await writeFile(join(packageRoot, 'skills', 'one', 'SKILL.md'), skill);
  await writeFile(join(packageRoot, 'skills', 'two', 'SKILL.md'), skill);

  await assert.rejects(
    () => loadSkillInventory({ packageRoot }),
    /duplicate skill frontmatter name: loam::duplicate/,
  );
});
