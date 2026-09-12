import assert from "node:assert/strict";
import test from "node:test";

import {
  mergeTauriUpdates,
  normalizeTauriUpdate,
} from "./normalize-tauri-update.mjs";

const requiredPlatforms = [
  "darwin-aarch64",
  "darwin-x86_64",
  "linux-aarch64",
  "linux-x86_64",
  "windows-aarch64",
  "windows-x86_64",
];

function releaseFixture() {
  return {
    tag_name: "v1.2.3",
    assets: requiredPlatforms.map((platform, index) => ({
      id: index + 1,
      name: `${platform}.tar.gz`,
      browser_download_url: `https://github.com/owner/repository/releases/download/v1.2.3/${platform}.tar.gz`,
    })),
  };
}

function manifestFixture() {
  return {
    version: "1.2.3",
    platforms: Object.fromEntries(
      requiredPlatforms.map((platform, index) => [
        platform,
        {
          signature: `signature-${platform}`,
          url: `https://api.github.com/repos/owner/repository/releases/assets/${index + 1}`,
        },
      ]),
    ),
  };
}

test("normalizes a complete six-platform updater manifest", () => {
  const manifest = manifestFixture();
  const normalized = normalizeTauriUpdate(
    manifest,
    releaseFixture(),
    "owner/repository",
    "1.2.3",
  );

  for (const platform of requiredPlatforms) {
    assert.equal(
      normalized.platforms[platform].url,
      `https://github.com/owner/repository/releases/download/v1.2.3/${platform}.tar.gz`,
    );
  }
});

test("merges updater manifests produced by parallel platform jobs", () => {
  const updates = requiredPlatforms.map((platform, index) => ({
    owner: platform,
    manifest: {
      version: "1.2.3",
      notes: "Release notes",
      pub_date: `2026-09-12T10:00:0${index}.000Z`,
      platforms: {
        [platform]: manifestFixture().platforms[platform],
      },
    },
  }));
  updates.at(-1).manifest.platforms["darwin-aarch64"] = {
    signature: "stale-signature",
    url: "https://api.github.com/repos/owner/repository/releases/assets/999",
  };

  const merged = mergeTauriUpdates(updates, "1.2.3");

  assert.deepEqual(Object.keys(merged.platforms).sort(), requiredPlatforms);
  assert.equal(merged.notes, "Release notes");
  assert.equal(merged.pub_date, "2026-09-12T10:00:05.000Z");
  assert.equal(
    merged.platforms["darwin-aarch64"].signature,
    "signature-darwin-aarch64",
  );
});

test("rejects an updater manifest missing a required platform", () => {
  const manifest = manifestFixture();
  delete manifest.platforms["windows-aarch64"];

  assert.throws(
    () =>
      normalizeTauriUpdate(
        manifest,
        releaseFixture(),
        "owner/repository",
        "1.2.3",
      ),
    /missing required platforms: windows-aarch64/,
  );
});

test("rejects merging updater manifests from another version", () => {
  const manifest = manifestFixture();
  manifest.version = "1.2.4";

  assert.throws(
    () =>
      mergeTauriUpdates(
        [{ owner: "darwin-aarch64", manifest }],
        "1.2.3",
      ),
    /updater manifest version 1\.2\.4; expected 1\.2\.3/,
  );
});
