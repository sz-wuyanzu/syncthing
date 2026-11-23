import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const version = "2.1.7";
const assets = {
  darwin: {
    arm64: {
      platform: "macos",
      architecture: "arm64",
      extension: "zip",
      sha256: "2df075b463231a307c91da657157beaed06383a941ba7ea04d33d536e3c4af38",
    },
    x64: {
      platform: "macos",
      architecture: "amd64",
      extension: "zip",
      sha256: "b8be90176d0f24daebae7b5aaddac52ebfae395df6d99f5e0e54656db00868cb",
    },
  },
  linux: {
    arm: {
      platform: "linux",
      architecture: "arm",
      extension: "tar.gz",
      sha256: "d8b59029e25992bc0d280fccc5626e5dca743b285fca1a36db1148fcf871b639",
    },
    arm64: {
      platform: "linux",
      architecture: "arm64",
      extension: "tar.gz",
      sha256: "48e456690cd2b97e7d26b02b5ac35f62a3b36172267b144cfeaf02c993ab955c",
    },
    ia32: {
      platform: "linux",
      architecture: "386",
      extension: "tar.gz",
      sha256: "3cbee24e509011d19859a2147f641b6cfff6e63e23ad3c30a410ea79f9322bac",
    },
    x64: {
      platform: "linux",
      architecture: "amd64",
      extension: "tar.gz",
      sha256: "2edc416e7ed136b7c858ed5f3f02a3b8316589605e54c380228ceea7acbf0142",
    },
  },
  win32: {
    arm64: {
      platform: "windows",
      architecture: "arm64",
      extension: "zip",
      sha256: "d535ffd611b3af223de64bc5dfc60c378881eb13d5c8ecb1431b7407094c15ff",
    },
    ia32: {
      platform: "windows",
      architecture: "386",
      extension: "zip",
      sha256: "8b3ef0563cc21d923c9faa4282f07c94eea039dba658ef435ab5ac99020ed122",
    },
    x64: {
      platform: "windows",
      architecture: "amd64",
      extension: "zip",
      sha256: "3ce7da770f546a8d02b65b2e7ee31f2a3b8b7c3f990ede045590cca4172ecfe1",
    },
  },
};

function run(command, args) {
  const result = spawnSync(command, args, { stdio: "inherit" });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    throw new Error(`${command} exited with status ${result.status}`);
  }
}

const release = assets[process.platform]?.[process.arch];
if (!release) {
  throw new Error(
    `Syncthing ${version} has no configured bundle for ${process.platform}/${process.arch}`,
  );
}

const archiveName = `syncthing-${release.platform}-${release.architecture}-v${version}.${release.extension}`;
const archiveUrl = `https://github.com/syncthing/syncthing/releases/download/v${version}/${archiveName}`;
const projectDirectory = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const resourceDirectory = join(projectDirectory, "src-tauri", "target", "bundled-resources");
const executableName = process.platform === "win32" ? "syncthing.exe" : "syncthing";
const executablePath = join(resourceDirectory, executableName);
const licensePath = join(resourceDirectory, "Syncthing-LICENSE.txt");
const markerPath = join(resourceDirectory, "syncthing-version.txt");
const marker = `${version} ${process.platform} ${process.arch}`;

if (
  existsSync(executablePath) &&
  existsSync(licensePath) &&
  existsSync(markerPath) &&
  readFileSync(markerPath, "utf8") === marker
) {
  console.log(`Syncthing ${version} is already staged for ${process.platform}/${process.arch}.`);
  process.exit(0);
}

const temporaryDirectory = mkdtempSync(join(tmpdir(), "syncthing-desktop-"));
try {
  const archivePath = join(temporaryDirectory, archiveName);
  const extractionDirectory = join(temporaryDirectory, "extracted");
  mkdirSync(extractionDirectory);
  run("curl", [
    "--fail",
    "--location",
    "--silent",
    "--show-error",
    "--retry",
    "3",
    "--output",
    archivePath,
    archiveUrl,
  ]);

  const digest = createHash("sha256").update(readFileSync(archivePath)).digest("hex");
  if (digest !== release.sha256) {
    throw new Error(
      `SHA-256 mismatch for ${archiveName}: expected ${release.sha256}, got ${digest}`,
    );
  }

  if (release.extension === "zip") {
    if (process.platform === "win32") {
      const quotePowerShellPath = (value) => value.replaceAll("'", "''");
      const command = [
        "$ErrorActionPreference = 'Stop'",
        `Expand-Archive -LiteralPath '${quotePowerShellPath(archivePath)}' -DestinationPath '${quotePowerShellPath(extractionDirectory)}' -Force`,
      ].join("; ");
      run("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", command]);
    } else {
      run("unzip", ["-q", archivePath, "-d", extractionDirectory]);
    }
  } else {
    run("tar", ["-xzf", archivePath, "-C", extractionDirectory]);
  }

  const extractedDirectory = join(
    extractionDirectory,
    `syncthing-${release.platform}-${release.architecture}-v${version}`,
  );
  const sourceExecutable = join(
    extractedDirectory,
    process.platform === "win32" ? "syncthing.exe" : "syncthing",
  );
  const sourceLicense = join(extractedDirectory, "LICENSE.txt");
  if (!existsSync(sourceExecutable) || !existsSync(sourceLicense)) {
    throw new Error(
      `The ${archiveName} archive does not contain the expected executable and LICENSE.txt`,
    );
  }

  rmSync(resourceDirectory, { recursive: true, force: true });
  mkdirSync(resourceDirectory, { recursive: true });
  copyFileSync(sourceExecutable, executablePath);
  copyFileSync(sourceLicense, licensePath);
  if (process.platform !== "win32") {
    chmodSync(executablePath, 0o755);
  }
  writeFileSync(markerPath, marker);
  console.log(`Staged Syncthing ${version} for ${process.platform}/${process.arch}.`);
} finally {
  rmSync(temporaryDirectory, { recursive: true, force: true });
}
