"""Bootstrap command contracts; no package installation, downloads, or live state."""
import json
import hashlib
import io
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import tarfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "scripts/linux-dev.sh"


class LinuxDevTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.log = self.root / "commands.jsonl"
        self.env = dict(os.environ, PATH=f"{self.bin}:/usr/bin:/bin", COMMAND_LOG=str(self.log))
        self.program("sudo")

    def program(self, name, body=""):
        path = self.bin / name
        path.write_text(
            f"#!{sys.executable}\nimport json, os, sys\nfrom pathlib import Path\n"
            "with open(os.environ['COMMAND_LOG'], 'a') as f:\n"
            "    f.write(json.dumps([Path(sys.argv[0]).name, *sys.argv[1:]]) + '\\n')\n"
            + body + "\n"
        )
        path.chmod(0o755)

    def run_shell(self, commands, success=True):
        result = subprocess.run(
            ["bash", "-c", f"source {shlex.quote(str(SCRIPT))}\n"
             f"repo_root={shlex.quote(str(self.root))}\n"
             'tools_dir="$repo_root/.dev-tools"\n' + commands],
            env=self.env, capture_output=True, text=True,
        )
        if success:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0)
        return result

    def commands(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def install_packages(self, distro):
        os_release = self.root / "os-release"
        os_release.write_text(distro)
        return self.run_shell(f"install_system_dependencies {shlex.quote(str(os_release))}")

    def test_arch_packages_and_no_partial_database_refresh(self):
        self.install_packages("ID=arch\n")
        command, = self.commands()
        self.assertEqual(command[:4], ["sudo", "pacman", "-S", "--needed"])
        for package in ("base-devel", "python-jinja", "python-yaml", "nodejs", "npm", "iproute2", "iputils"):
            self.assertIn(package, command)

    def test_debian_packages(self):
        self.install_packages("ID=debian\n")
        update, install = self.commands()
        self.assertEqual(update, ["sudo", "apt-get", "update"])
        self.assertEqual(install[:4], ["sudo", "apt-get", "install", "--no-install-recommends"])
        for package in ("build-essential", "python3-jinja2", "python3-yaml", "libssl-dev", "nodejs", "npm"):
            self.assertIn(package, install)

    def test_family_detection_and_managed_tool_preference(self):
        self.program("mise")
        self.program("uv")
        self.install_packages('ID=linuxmint\nID_LIKE="ubuntu debian"\n')
        install = self.commands()[-1]
        for package in ("nodejs", "npm", "python3", "python3-jinja2", "python3-yaml"):
            self.assertNotIn(package, install)
        self.assertIn("nginx", install)

    def test_unknown_distribution_does_not_install(self):
        os_release = self.root / "os-release"
        os_release.write_text("ID=fedora\n")
        result = self.run_shell(f"install_system_dependencies {shlex.quote(str(os_release))}", success=False)
        self.assertIn("Arch and Debian", result.stderr)
        self.assertEqual(self.commands(), [])

    def test_mise_and_uv_toolchains(self):
        self.program("mise", "if 'rustc' in sys.argv: print('rustc 1.85.0 (fixture)')")
        self.program("uv", "if sys.argv[1] == 'venv':\n"
                     "    dest = Path(sys.argv[-1]) / 'bin'\n"
                     "    dest.mkdir(parents=True)\n"
                     "    (dest / 'python3').symlink_to(os.environ['MOCK_PYTHON'])")
        self.program("python3")
        self.env["MOCK_PYTHON"] = str(self.bin / "python3")
        self.run_shell('export PATH="$tools_dir/python/bin:$PATH"\ninstall_toolchains')
        commands = self.commands()
        self.assertEqual(commands[0], ["mise", "install", "rust@stable", "node@22"])
        self.assertIn(["mise", "exec", "rust@stable", "--", "rustc", "--version"], commands)
        self.assertIn(["uv", "venv", "--python", "3.11", str(self.root / ".dev-tools/python")], commands)
        self.assertIn(["uv", "pip", "install", "--python", str(self.root / ".dev-tools/python/bin/python3"),
                       "-r", str(self.root / "requirements.txt")], commands)
        self.assertFalse(any(c[0] in ("sudo", "curl", "rustup") for c in commands))
        self.log.unlink()
        self.run_shell('export PATH="$tools_dir/python/bin:$PATH"\ninstall_toolchains')
        self.assertFalse(any(c[:2] == ["uv", "venv"] for c in self.commands()))

    def test_existing_toolchains_need_no_download(self):
        self.program("rustc", "print('rustc 1.85.0 (fixture)')")
        self.program("cargo")
        self.program("node")
        self.program("npm")
        self.program("python3")
        self.run_shell("install_toolchains")
        self.assertFalse(any(c[0] in ("curl", "rustup", "sudo") for c in self.commands()))

    def test_old_rust_uses_rustup_without_changing_default(self):
        self.program("rustc", "print('rustc 1.85.0 (fixture)' if os.environ.get('RUSTUP_TOOLCHAIN') == 'stable' else 'rustc 1.63.0 (fixture)')")
        for name in ("cargo", "rustup", "node", "npm", "python3"):
            self.program(name)
        self.run_shell("install_toolchains")
        self.assertIn(["rustup", "toolchain", "install", "stable", "--profile", "minimal"], self.commands())
        self.assertFalse(any(c[:2] == ["rustup", "default"] for c in self.commands()))

    def node_archive(self, corrupt=False):
        name = "node-v22.12.0-linux-x64.tar.xz"
        archive = self.root / name
        with tarfile.open(archive, "w:xz") as bundle:
            info = tarfile.TarInfo("node-v22.12.0-linux-x64/bin/node")
            content = b"verified fixture"
            info.size = len(content)
            bundle.addfile(info, io.BytesIO(content))
        checksum = "0" * 64 if corrupt else hashlib.sha256(archive.read_bytes()).hexdigest()
        self.env["NODE_FIXTURE"] = str(archive)
        self.env["NODE_CHECKSUM"] = checksum
        self.program("uname", "print('x86_64')")
        self.program("curl", "import shutil\n"
                     "dest = Path(sys.argv[sys.argv.index('-o') + 1])\n"
                     "if dest.name == 'SHASUMS256.txt':\n"
                     "    dest.write_text(os.environ['NODE_CHECKSUM'] + '  ' + Path(os.environ['NODE_FIXTURE']).name + '\\n')\n"
                     "else:\n"
                     "    shutil.copyfile(os.environ['NODE_FIXTURE'], dest)")

    def test_node_fallback_verifies_archive_before_installing(self):
        self.node_archive()
        self.run_shell("install_node_fallback")
        self.assertEqual((self.root / ".dev-tools/node/bin/node").read_bytes(), b"verified fixture")
        self.assertFalse(any(c[0] == "sudo" for c in self.commands()))

    def test_node_checksum_failure_does_not_install(self):
        self.node_archive(corrupt=True)
        self.run_shell("install_node_fallback", success=False)
        self.assertFalse((self.root / ".dev-tools/node").exists())

    def test_init_runs_setup_and_npm_after_toolchains(self):
        self.program("id", "print('1000')")
        self.program("python3")
        self.program("mise", "import subprocess\n"
                     "if 'rustc' in sys.argv: print('rustc 1.85.0 (fixture)')\n"
                     "if 'bash' in sys.argv:\n"
                     "    sys.exit(subprocess.call(sys.argv[sys.argv.index('--') + 1:]))")
        self.program("setup-fixture")
        scripts = self.root / "scripts"
        scripts.mkdir()
        (scripts / "setup-linux.sh").write_text('#!/bin/bash\nsetup-fixture\n')
        self.run_shell('install_system_dependencies() { sudo fixture-packages; }\nmain init')
        commands = self.commands()
        packages = commands.index(["sudo", "fixture-packages"])
        toolchains = commands.index(["mise", "install", "rust@stable", "node@22"])
        setup = commands.index(["setup-fixture"])
        npm = commands.index(["mise", "exec", "node@22", "--", "npm", "--prefix", str(self.root / "linux-web"), "ci"])
        self.assertLess(packages, toolchains)
        self.assertLess(toolchains, setup)
        self.assertLess(setup, npm)

    def test_node_version_boundaries(self):
        # Use real Node, if installed, to evaluate the exact Vite version check.
        for version, accepted in [("18.20.0", False), ("20.18.0", False), ("20.19.0", True),
                                  ("22.11.0", False), ("22.12.0", True), ("24.0.0", True)]:
            self.program("node", "os.execv('/usr/bin/node', ['node', '-e', "
                         + repr(f"Object.defineProperty(process.versions, 'node', {{value: '{version}'}}); ")
                         + " + sys.argv[2]])")
            self.program("npm")
            if not Path("/usr/bin/node").exists():
                self.skipTest("Node is not installed")
            self.run_shell("check_node", success=accepted)

    def test_run_builds_ui_then_uses_mise_rust(self):
        self.program("id", "print('1000')")
        self.program("mise", "if 'rustc' in sys.argv: print('rustc 1.85.0 (fixture)')")
        self.run_shell("main run")
        commands = self.commands()
        build = ["mise", "exec", "node@22", "--", "npm", "--prefix", str(self.root / "linux-web"), "run", "build"]
        serve = next(c for c in commands if "serve" in c)
        self.assertLess(commands.index(build), commands.index(serve))
        self.assertEqual(serve[:6], ["mise", "exec", "rust@stable", "--", "cargo", "run"])
        self.assertIn("linux-ui", serve)
        self.assertNotIn("--test-mode", serve)

    def test_root_is_rejected_before_any_changes(self):
        self.program("id", "print('0')")
        result = self.run_shell("main init", success=False)
        self.assertIn("non-root", result.stderr)
        self.assertEqual(self.commands(), [["id", "-u"]])

    def test_setup_with_installed_dehydrated_creates_directories_and_preserves_files(self):
        # Run the actual setup script in a fresh temporary repository. The old
        # ordering only made certs/ when dehydrated was absent from PATH.
        scripts = self.root / "scripts"
        scripts.mkdir()
        shutil.copyfile(SCRIPT.with_name("setup-linux.sh"), scripts / "setup-linux.sh")
        (scripts / "dehydrated-dns01-hook.sh").write_text("bundled hook\n")
        (scripts / "linux-init.py").write_text("init adapter\n")
        for name in ("cargo", "nginx", "curl", "dehydrated"):
            self.program(name)
        self.program("python3", "if len(sys.argv) > 3 and sys.argv[1].endswith('render-nginx-config.py'):\n"
                     "    nginx = Path(sys.argv[3])\n"
                     "    (nginx / 'certs/default').mkdir(parents=True)\n"
                     "    (nginx / 'nginx.conf').write_text('rendered nginx config')")
        self.program("openssl", "if sys.argv[1] == 'rand': print('test-token-' + 'a' * 40)\n"
                     "if sys.argv[1] == 'req':\n"
                     "    for flag in ('-keyout', '-out'):\n"
                     "        Path(sys.argv[sys.argv.index(flag) + 1]).write_text('certificate fixture')")
        setup = f"bash {shlex.quote(str(scripts / 'setup-linux.sh'))}"
        self.run_shell(setup)
        state = self.root / "data/linux"
        hook = state / "certs/dehydrated-dns01-hook.sh"
        self.assertEqual(hook.read_text(), "bundled hook\n")
        config = state / "maestro.yaml"
        original_config = config.read_bytes()
        self.assertEqual(json.loads(original_config)["commands"]["dehydrated"], str(self.bin / "dehydrated"))
        self.assertEqual(state.stat().st_mode & 0o777, 0o700)
        self.assertTrue((state / "init.d/S80nginx").exists())
        hook.write_text("operator hook\n")
        self.run_shell(setup)
        self.assertEqual(config.read_bytes(), original_config)
        self.assertEqual(hook.read_text(), "operator hook\n")
        self.assertFalse(any(c[0] == "curl" for c in self.commands()))


if __name__ == "__main__":
    unittest.main()
