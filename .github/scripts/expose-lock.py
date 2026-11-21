# Temporary CI transport for retrieving Cargo.lock through GitHub check annotations.
import base64
import gzip
import sys
content = base64.b64encode(gzip.compress(open('src-tauri/Cargo.lock','rb').read(), mtime=0)).decode()
parts = [content[i:i+3500] for i in range(0, len(content), 3500)]
group = int(sys.argv[1])
for i, part in enumerate(parts[group*10:(group+1)*10], group*10):
    print(f'::notice title=HorizonLock{i:03d}::{part}')
if group == 0:
    print(f'::notice title=HorizonLockCount::{len(parts)}')
