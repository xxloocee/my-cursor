import { readFile, readdir, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const requiredPlatforms = [
  "darwin-aarch64",
  "darwin-x86_64",
  "linux-aarch64",
  "linux-x86_64",
  "windows-aarch64",
  "windows-x86_64",
];

const manifestOwners = new Map([
  ["tauri-update-macos-aarch64", "darwin-aarch64"],
  ["tauri-update-macos-x86_64", "darwin-x86_64"],
  ["tauri-update-linux-aarch64", "linux-aarch64"],
  ["tauri-update-linux-x86_64", "linux-x86_64"],
  ["tauri-update-windows-aarch64", "windows-aarch64"],
  ["tauri-update-windows-x86_64", "windows-x86_64"],
]);

function readOptions(args) {
  const options = new Map();
  for (let index = 0; index < args.length; index += 2) {
    const name = args[index];
    const value = args[index + 1];
    if (!name?.startsWith("--") || value === undefined) {
      throw new Error(`invalid argument near ${name ?? "end of command"}`);
    }
    options.set(name.slice(2), value);
  }
  return options;
}

function required(options, name) {
  const value = options.get(name)?.trim();
  if (!value) throw new Error(`--${name} is required`);
  return value;
}

export function mergeTauriUpdates(updates, version) {
  if (updates.length === 0) {
    throw new Error("no Tauri updater manifests were found");
  }

  const merged = {
    version,
    notes: updates[0].manifest.notes,
    pub_date: updates
      .map(({ manifest }) => manifest.pub_date)
      .filter(Boolean)
      .sort()
      .at(-1),
    platforms: {},
  };

  for (const { owner, manifest } of updates) {
    if (!requiredPlatforms.includes(owner)) {
      throw new Error(`unknown updater manifest owner: ${owner}`);
    }
    if (manifest.version !== version) {
      throw new Error(
        `updater manifest version ${manifest.version ?? "is missing"}; expected ${version}`,
      );
    }
    if (!manifest.platforms || typeof manifest.platforms !== "object") {
      throw new Error("updater manifest has no platforms");
    }
    if (!Object.hasOwn(manifest.platforms, owner)) {
      throw new Error(`updater manifest for ${owner} has no base platform entry`);
    }

    for (const [platform, entry] of Object.entries(manifest.platforms)) {
      if (platform === owner || platform.startsWith(`${owner}-`)) {
        merged.platforms[platform] = entry;
      }
    }
  }

  return merged;
}

async function readTauriUpdates(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const manifests = entries
    .filter((entry) => entry.isDirectory())
    .map((entry) => ({
      name: entry.name,
      path: join(directory, entry.name, "latest.json"),
    }))
    .sort(({ name: left }, { name: right }) => left.localeCompare(right));
  return Promise.all(
    manifests.map(async ({ name, path }) => ({
      owner: manifestOwners.get(name),
      manifest: JSON.parse(await readFile(path, "utf8")),
    })),
  );
}

export function normalizeTauriUpdate(manifest, release, repository, version) {
  if (manifest.version !== version) {
    throw new Error(
      `updater manifest version ${manifest.version ?? "is missing"}; expected ${version}`,
    );
  }
  if (release.tag_name !== `v${version}`) {
    throw new Error(
      `GitHub release tag ${release.tag_name ?? "is missing"}; expected v${version}`,
    );
  }
  if (!manifest.platforms || typeof manifest.platforms !== "object") {
    throw new Error("updater manifest has no platforms");
  }

  const missingPlatforms = requiredPlatforms.filter(
    (platform) => !manifest.platforms[platform],
  );
  if (missingPlatforms.length > 0) {
    throw new Error(
      `updater manifest is missing required platforms: ${missingPlatforms.join(", ")}`,
    );
  }

  const assetsBySourceUrl = new Map();
  const publicAssetUrls = new Set();
  for (const asset of release.assets ?? []) {
    if (!asset?.id || !asset?.name) continue;
    const publicUrl = `https://github.com/${repository}/releases/download/v${version}/${encodeURIComponent(asset.name)}`;
    assetsBySourceUrl.set(
      `https://api.github.com/repos/${repository}/releases/assets/${asset.id}`,
      publicUrl,
    );
    if (asset.browser_download_url) {
      assetsBySourceUrl.set(asset.browser_download_url, publicUrl);
    }
    publicAssetUrls.add(publicUrl);
  }

  for (const [platform, entry] of Object.entries(manifest.platforms)) {
    if (!entry?.signature || !entry?.url) {
      throw new Error(`updater platform ${platform} is missing its URL or signature`);
    }
    const publicUrl = assetsBySourceUrl.get(entry.url) ?? entry.url;
    if (!publicAssetUrls.has(publicUrl)) {
      throw new Error(`updater platform ${platform} references an unknown release asset`);
    }
    entry.url = publicUrl;
  }

  return manifest;
}

async function main() {
  const options = readOptions(process.argv.slice(2));
  const manifestsDir = resolve(required(options, "manifests-dir"));
  const manifestPath = resolve(required(options, "manifest"));
  const releasePath = resolve(required(options, "release"));
  const repository = required(options, "repository");
  const version = required(options, "version").replace(/^v/, "");

  if (!/^[^/\s]+\/[^/\s]+$/.test(repository)) {
    throw new Error(`invalid GitHub repository: ${repository}`);
  }
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version)) {
    throw new Error(`invalid semantic version: ${version}`);
  }

  const manifest = mergeTauriUpdates(
    await readTauriUpdates(manifestsDir),
    version,
  );
  const release = JSON.parse(await readFile(releasePath, "utf8"));
  const normalized = normalizeTauriUpdate(
    manifest,
    release,
    repository,
    version,
  );
  await writeFile(manifestPath, `${JSON.stringify(normalized, null, 2)}\n`);
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  main().catch((error) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}
