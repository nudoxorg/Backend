// Populate the graph prototype's local source links for this checkout.
// Never replace an existing path or copy a machine-specific Cargo cache.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '../../..');
const repo = path.join(here, 'repo');
const cargoHome = path.resolve(process.env.CARGO_HOME || path.join(os.homedir(), '.cargo'));
const registryRoot = path.join(cargoHome, 'registry', 'src');
const links = ['apps', 'crates', 'extensions', 'frontends'].map(name =>
  [path.join(repo, name), path.join(root, name)]);
if (fs.existsSync(registryRoot)) {
  const registry = fs.readdirSync(registryRoot, { withFileTypes: true })
    .filter(entry => entry.isDirectory() && entry.name.startsWith('index.crates.io-'))
    .map(entry => entry.name).sort()[0];
  if (registry) links.push([path.join(here, 'registry'), path.join(registryRoot, registry)]);
}
for (const [link, target] of links) {
  if (!fs.statSync(target).isDirectory()) throw new Error(`Not a directory: ${target}`);
  let exists = true;
  try { fs.lstatSync(link); }
  catch (error) {
    if (error.code !== 'ENOENT') throw error;
    exists = false;
  }
  if (exists) {
    let actual;
    try { actual = fs.realpathSync(link); }
    catch { throw new Error(`Refusing to replace ${link}`); }
    if (actual !== fs.realpathSync(target)) throw new Error(`Refusing to replace ${link}`);
  }
}
fs.mkdirSync(repo, { recursive: true });
for (const [link, target] of links) {
  try { fs.lstatSync(link); }
  catch (error) {
    if (error.code !== 'ENOENT') throw error;
    fs.symlinkSync(target, link, process.platform === 'win32' ? 'junction' : 'dir');
  }
}
console.log(`Configured ${links.length} local source links.`);
if (links.length === 4) console.log('Cargo registry sources not found; external source links remain unavailable.');
