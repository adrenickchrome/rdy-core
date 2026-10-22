"""Failure-path checks for the CI scripts, with no network or product store."""
import hashlib
import importlib.util
import io
from pathlib import Path
import tarfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts/ci' / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


TOOLS = load('install_tools')


class ToolArchive(unittest.TestCase):
    @staticmethod
    def archive(entries):
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode='w:gz') as tar:
            for name, kind in entries:
                member = tarfile.TarInfo(name)
                if kind == 'file':
                    member.size = 4
                    tar.addfile(member, io.BytesIO(b'tool'))
                elif kind == 'link':
                    member.type = tarfile.SYMTYPE
                    member.linkname = '/tmp/outside'
                    tar.addfile(member)
                else:
                    member.type = tarfile.CHRTYPE
                    tar.addfile(member)
        return output.getvalue()

    def extract(self, entries):
        data = self.archive(entries)
        return TOOLS.extract_binary(data, hashlib.sha256(data).hexdigest(), 'tool')

    def test_regular_exact_binary_only(self):
        self.assertEqual(self.extract([('package/LICENSE', 'file'), ('package/tool', 'file')]), b'tool')

    def test_bad_hash_refused_before_tar(self):
        with self.assertRaises(ValueError):
            TOOLS.extract_binary(b'not-an-archive', '0' * 64, 'tool')

    def test_path_escape_links_devices_duplicates_missing_refused(self):
        for entries in [[('../tool', 'file')], [('/tool', 'file')], [('tool', 'link')],
                        [('tool', 'device')], [('x/tool', 'file'), ('y/tool', 'file')], [('other', 'file')]]:
            with self.assertRaises(ValueError):
                self.extract(entries)


if __name__ == '__main__':
    unittest.main()
