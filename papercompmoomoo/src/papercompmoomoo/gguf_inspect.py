"""Read GGUF v2/v3 metadata and tensor sizes without loading weights."""
import math
import struct
from pathlib import Path

def inspect_gguf(path):
    sizes={0:1,1:1,2:2,3:2,4:4,5:4,6:4,7:1,10:8,11:8,12:8}
    formats={0:'B',1:'b',2:'H',3:'h',4:'I',5:'i',6:'f',7:'?',10:'Q',11:'q',12:'d'}
    with Path(path).open('rb') as f:
        def number(fmt):
            size=struct.calcsize('<'+fmt);raw=f.read(size)
            if len(raw)!=size:raise ValueError('truncated GGUF')
            return struct.unpack('<'+fmt,raw)[0]
        def string(keep=True):
            n=number('Q')
            if n>128*1024*1024:raise ValueError('invalid GGUF string length')
            if keep:
                raw=f.read(n)
                if len(raw)!=n:raise ValueError('truncated GGUF string')
                return raw.decode('utf-8')
            f.seek(n,1)
        def value(kind,keep=False):
            if kind in formats:
                if keep:return number(formats[kind])
                f.seek(sizes[kind],1);return
            if kind==8:return string(keep)
            if kind==9:
                subtype=number('I');count=number('Q')
                if count>10000000:raise ValueError('invalid GGUF array length')
                if subtype in sizes:f.seek(count*sizes[subtype],1)
                else:
                    for _ in range(count):value(subtype)
                return
            raise ValueError('unsupported GGUF metadata type '+str(kind))
        if f.read(4)!=b'GGUF':raise ValueError('not a GGUF file')
        version=number('I')
        if version not in (2,3):raise ValueError('unsupported GGUF version')
        tensors=number('Q');pairs=number('Q')
        if tensors>1000000 or pairs>100000:raise ValueError('invalid GGUF header counts')
        metadata={}
        for _ in range(pairs):
            key=string();kind=number('I');keep=key in ('general.architecture','general.name','general.file_type') or key.endswith(('.embedding_length','.block_count','.feed_forward_length'))
            found=value(kind,keep)
            if keep:metadata[key]=found
        parameters=0
        for _ in range(tensors):
            string(False);dimensions=number('I')
            if not 1<=dimensions<=4:raise ValueError('invalid GGUF tensor dimensions')
            parameters+=math.prod(number('Q') for _ in range(dimensions))
            number('I');number('Q')
        return {'version':version,'tensor_count':tensors,'parameter_count':parameters,'metadata':metadata,
            'file_bytes':Path(path).stat().st_size,'float32_weight_bytes':parameters*4}
