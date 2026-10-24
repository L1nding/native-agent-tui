"""Package the Windows TUI, dsh/Node and the native modeling Python runtime."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import zipfile


REPO = Path(__file__).resolve().parents[1]


def bundle_modeler(source, destination):
    source = source.resolve()
    interpreter = source / ".venv/Scripts/python.exe"
    if not interpreter.is_file():
        raise ValueError("modeler build interpreter missing: " + str(interpreter))
    query = r'''
import importlib.metadata as metadata
import json, sys, sysconfig
from pathlib import Path
from packaging.requirements import Requirement
from packaging.utils import canonicalize_name
pending = [(name, frozenset()) for name in ("pydantic", "shapely", "ezdxf", "PyYAML", "mcp", "pillow")]
seen, distributions = set(), {}
while pending:
    name, extras = pending.pop()
    key = canonicalize_name(name), extras
    if key in seen:
        continue
    seen.add(key)
    distribution = metadata.distribution(name)
    distributions[key[0]] = {"version": distribution.version,
        "files": [str(file).replace("\\", "/") for file in distribution.files or ()]}
    for raw in distribution.requires or ():
        requirement = Requirement(raw)
        if requirement.marker is None or any(requirement.marker.evaluate({"extra": extra}) for extra in ("", *extras)):
            pending.append((requirement.name, frozenset(requirement.extras)))
print(json.dumps({"base": sys.base_prefix, "site": sysconfig.get_path("purelib"),
                  "version": sys.version.split()[0], "distributions": distributions}))
'''
    metadata = json.loads(subprocess.check_output([str(interpreter), "-I", "-c", query], text=True))
    base, site = Path(metadata["base"]), Path(metadata["site"])
    python = destination / "py"
    python.mkdir(parents=True)

    def exclude_runtime(directory, names):
        return [name for name in names if name in {"__pycache__", "site-packages", "test", "tests"}
                or name.endswith((".pyc", ".pyo")) or (Path(directory) / name).is_symlink()]

    for name in ("Lib", "DLLs", "tcl"):
        if (base / name).is_dir():
            shutil.copytree(base / name, python / name, ignore=exclude_runtime)
    for path in base.iterdir():
        if path.is_file() and (path.suffix.lower() in {".exe", ".dll"} or path.name == "LICENSE.txt"):
            shutil.copy2(path, python / path.name)
    installed = python / "Lib/site-packages"
    installed.mkdir()
    for distribution in metadata["distributions"].values():
        for member in distribution["files"]:
            relative = Path(member)
            original = (site / relative).resolve()
            if not original.is_relative_to(site) or not original.is_file():
                continue
            if any(part in {"__pycache__", "test", "tests", "testing"} for part in relative.parts):
                continue
            if relative.suffix in {".pyc", ".pyo", ".pth"} or relative.name in {"direct_url.json", "RECORD"}:
                continue
            target = installed / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(original, target)
    # 相对路径随整个包搬移；不复制 editable/venv 启动器或构建机路径。
    (installed / "modeler.pth").write_text("../../../src\n", encoding="utf-8")
    if "pywin32" in metadata["distributions"]:
        (installed / "pywin32.pth").write_text(
            "win32\nwin32/lib\nPythonwin\nimport pywin32_bootstrap\n", encoding="utf-8")
    shutil.copytree(source / "src/blender_agent", destination / "src/blender_agent", ignore=exclude_runtime)
    shutil.copytree(source / "review-web/dist", destination / "review-web/dist", ignore=exclude_runtime)
    shutil.copytree(source / ".agents/skills", destination / ".agents/skills", ignore=exclude_runtime)
    (destination / "docs").mkdir()
    shutil.copy2(source / "docs/agent-workflow.md", destination / "docs/agent-workflow.md")
    (destination / "scripts").mkdir()
    shutil.copy2(source / "scripts/blender_preview.py", destination / "scripts/blender_preview.py")
    (destination / "projects").mkdir()
    (destination / "component-library").mkdir()
    (destination / "runtime.json").write_text(json.dumps({
        "python": metadata["version"],
        "packages": {name: value["version"] for name, value in metadata["distributions"].items()},
    }, indent=2) + "\n", encoding="utf-8")
    environment = dict(os.environ)
    for key in ("PYTHONHOME", "PYTHONPATH"):
        environment.pop(key, None)
    environment["PYTHONNOUSERSITE"] = "1"
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    subprocess.run([str(python / "python.exe"), "-c",
                    "import sys, ezdxf, shapely, pydantic, yaml, mcp, PIL, blender_agent; "
                    "import pywintypes, win32api, win32job, pythoncom; "
                    "from blender_agent.integrations.project_mcp import create_server; "
                    "print('Verified portable modeling Python '+sys.version.split()[0])"],
                   cwd=destination, env=environment, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dsh-root", type=Path, default=Path(os.environ.get("APPDATA", "")) / "dsh-tauri/dependencies/dsh")
    parser.add_argument("--node", type=Path, default=shutil.which("node.exe"))
    parser.add_argument("--modeler-fixes", type=Path, help="Include the already accepted modeling-service patch bundles")
    parser.add_argument("--modeler-root", type=Path, default=REPO.parent / "blender-agent-modeler")
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("this package requires Windows x64")
    if args.node is None or not args.node.is_file():
        parser.error("pass --node with the Windows x64 node.exe to bundle")
    node = args.node.resolve()
    modules = args.dsh_root.resolve() / "node_modules"
    manifest = json.loads((modules / "@deepseek-ai/dsh/package.json").read_text(encoding="utf-8"))
    if manifest["version"] != "0.1.7-rc.2":
        parser.error("the checked host repairs require dsh 0.1.7-rc.2")
    arch = subprocess.check_output([str(node), "-p", "process.arch"], text=True).strip()
    if arch != "x64":
        parser.error("the bundled Node runtime must be Windows x64")

    subprocess.run(["cargo", "build", "--locked", "--release", "--bin", "native-agent-tui"], cwd=REPO, check=True)
    subprocess.run([shutil.which("pnpm.cmd") or "pnpm", "run", "build"], cwd=REPO / "dsh-plugin", check=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S")
    output = REPO / "target/packages" / stamp
    output.mkdir(parents=True)
    root = REPO / "target/portable" / stamp / "zf-tui"
    runtime = root / "r"
    runtime.mkdir(parents=True)
    shutil.copy2(REPO / "target/release/native-agent-tui.exe", root)
    shutil.copy2(node, runtime / "node.exe")
    shutil.copy2(node.parent / "LICENSE", runtime / "NODE-LICENSE.txt")
    bundle_modeler(args.modeler_root, root / "m")

    def exclude(directory, names):
        return [name for name in names if (Path(directory).name == "node_modules" and name.startswith("."))
                or name == ".dsh-mattskillsdeck-cache"
                or (Path(directory) / name).is_symlink() or (Path(directory) / name).is_junction()]

    # Only installed package bytes: private desktop plugin links and pnpm stores
    # are not runtime dependencies and must not point outside the portable tree.
    destination = runtime / "d/node_modules"
    shutil.copytree("\\\\?\\" + str(modules), "\\\\?\\" + str(destination), ignore=exclude)
    bridge = destination / "zf-tui-bridge"
    bridge.mkdir()
    for name in ("package.json", "cordis.patch.yml"):
        shutil.copy2(REPO / "dsh-plugin" / name, bridge / name)
    shutil.copytree(REPO / "dsh-plugin/lib", bridge / "lib")
    host_manifest = destination / "@deepseek-ai/dsh/package.json"
    host = json.loads(host_manifest.read_text(encoding="utf-8"))
    host["dependencies"]["zf-tui-bridge"] = json.loads((bridge / "package.json").read_text(encoding="utf-8"))["version"]
    host_manifest.write_text(json.dumps(host, indent=2) + "\n", encoding="utf-8")

    # Regenerate from the installer's sealed originals, with an ESM-relative
    # import so moving/extracting the package cannot retain a build-machine URL.
    entry = destination / "@deepseek-ai/dsh/lib/bin.js"
    installer = (REPO / "scripts/install-dsh-context.mjs").as_uri()
    program = (
        "import {prepareRepairs,commitRepairs} from " + json.dumps(installer) + ";"
        "const plans=prepareRepairs(process.argv[1], '../../../zf-tui-bridge/lib/context.js');"
        "commitRepairs(plans);commitRepairs(plans,{check:true});"
        "console.log('Verified portable host repairs: '+plans.length);"
    )
    subprocess.run([str(runtime / "node.exe"), "--input-type=module", "-e", program, str(entry)], check=True)

    (runtime / "dsh-entry.mjs").write_text('''import { existsSync, mkdirSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const runtime = dirname(fileURLToPath(import.meta.url))
const root = dirname(runtime)
process.env.DSH_HOME = join(root, 'data', 'dsh')
const profile = join(process.env.DSH_HOME, 'profiles', 'zf-tui')
mkdirSync(profile, { recursive: true })
const manifest = join(profile, 'package.json')
if (!existsSync(manifest)) writeFileSync(manifest, JSON.stringify({
  name: 'dsh-profile-zf-tui', private: true,
  dsh: { profile: { bundles: ['@deepseek-ai/dsh-base', 'zf-tui-bridge'] } }
}, null, 2) + '\\n', { flag: 'wx' })
const entry = join(runtime, 'd', 'node_modules', '@deepseek-ai', 'dsh', 'lib', 'bin.js')
process.argv[1] = entry
const { runCli } = await import(pathToFileURL(entry).href)
await runCli()
''', encoding="utf-8")
    (runtime / "dsh.cmd").write_bytes(b'@echo off\r\n"%~dp0node.exe" "%~dp0dsh-entry.mjs" %*\r\n')
    (runtime / "launch.mjs").write_text('''import { spawnSync } from 'node:child_process'
import { dirname, delimiter, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const runtime = dirname(fileURLToPath(import.meta.url))
const root = dirname(runtime)
const args = process.argv.slice(2)
if (!args.some(arg => arg === '--cwd' || arg.startsWith('--cwd='))) args.unshift('--cwd', join(root, 'm'))
if (!args.includes('--journal-dir')) args.unshift('--journal-dir', join(root, 'data', 'journal'))
const env = { ...process.env, DSH_BIN: join(runtime, 'dsh.cmd'),
  DSH_HOME: join(root, 'data', 'dsh'), PYTHONNOUSERSITE: '1', PYTHONUTF8: '1',
  PYTHONDONTWRITEBYTECODE: '1',
  PATH: runtime + delimiter + join(root, 'm', 'py') + delimiter + (process.env.PATH ?? '') }
delete env.PYTHONHOME
delete env.PYTHONPATH
const result = spawnSync(join(root, 'native-agent-tui.exe'), args, {
  stdio: 'inherit',
  env
})
if (result.error) console.error(result.error.message)
process.exitCode = result.status ?? 1
''', encoding="utf-8")
    (root / "Start.cmd").write_bytes(b'@echo off\r\nsetlocal\r\n"%~dp0r\\node.exe" "%~dp0r\\launch.mjs" %*\r\nset "taskExit=%errorlevel%"\r\nif not "%taskExit%"=="0" pause\r\nexit /b %taskExit%\r\n')
    shutil.copy2(REPO / "README.md", root / "README.md")
    if args.modeler_fixes is not None:
        for package in args.modeler_fixes.iterdir():
            manifest = json.loads((package / "files.sha256.json").read_text(encoding="utf-8"))
            for entry in manifest["runtime_sync"]:
                path = package / "runtime-after" / entry["path"]
                if hashlib.sha256(path.read_bytes()).hexdigest() != entry["after_sha256"]:
                    raise ValueError("modeling patch hash mismatch: " + str(path))
        shutil.copytree(args.modeler_fixes, root / "modeler-fixes")

    scan_root = Path("\\\\?\\" + str(root))
    files = sorted(path for path in scan_root.rglob("*") if path.is_file())
    if any(".dsh-mattskillsdeck-cache" in path.parts for path in files):
        raise ValueError("host runtime cache was included in the package")
    checksums = "".join(hashlib.sha256(path.read_bytes()).hexdigest() + "  "
                        + path.relative_to(scan_root).as_posix() + "\n" for path in files)
    (root / "SHA256SUMS").write_text(checksums, encoding="utf-8")
    entries = [(path, (Path(root.name) / path.relative_to(scan_root)).as_posix())
               for path in files + [scan_root / "SHA256SUMS"]]
    # Leave room for the user's Downloads directory and Explorer's default
    # extraction folder; extended-path build I/O alone does not ensure this.
    longest = max((name for _, name in entries), key=len)
    if len(longest) > 180:
        raise ValueError("ZIP member path exceeds 180 characters: " + longest)
    archive = output / "zf-tui-win64.zip"
    with zipfile.ZipFile(archive, "x", zipfile.ZIP_DEFLATED, compresslevel=6) as bundle:
        for path, name in entries:
            bundle.write(path, name)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    Path(str(archive) + ".sha256").write_text(digest + "  " + archive.name + "\n", encoding="utf-8")
    print("PACKAGE " + str(archive), flush=True)
    print("ROOT " + str(root), flush=True)
    print("SHA256 " + digest, flush=True)


if __name__ == "__main__":
    main()
