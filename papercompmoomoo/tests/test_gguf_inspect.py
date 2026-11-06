import struct
import tempfile
import unittest
from pathlib import Path
from papercompmoomoo.gguf_inspect import inspect_gguf
from tools.qwen_train import preflight

class GGUFTests(unittest.TestCase):
    def fixture(self,path):
        def text(s):
            b=s.encode();return struct.pack('<Q',len(b))+b
        # Metadata-only fixture for parser tests, not a loadable model.
        path.write_bytes(b'GGUF'+struct.pack('<IQQ',3,1,1)+text('general.architecture')+struct.pack('<I',8)+text('qwen2')+
            text('blk.0.attn_q.weight')+struct.pack('<IQQIQ',2,4,8,0,0))

    def test_counts_parameters_without_loading_tensor_data(self):
        with tempfile.TemporaryDirectory() as tmp:
            p=Path(tmp)/'test.gguf';self.fixture(p);info=inspect_gguf(p)
            self.assertEqual(info['parameter_count'],32)
            self.assertEqual(info['float32_weight_bytes'],128)
            self.assertEqual(info['metadata']['general.architecture'],'qwen2')
            check=preflight(p)
            self.assertEqual(check['backend'],'gguf-dequantized-cpu-lora')
            self.assertFalse(any('Hugging Face model directory' in s for s in check['blockers']))

    def test_truncated_header_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            p=Path(tmp)/'bad.gguf';p.write_bytes(b'GGUF')
            with self.assertRaises(ValueError):inspect_gguf(p)
