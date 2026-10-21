"""Copy bundled runtime notices alongside the preview, without installing anything."""
from importlib import metadata
from pathlib import Path
import shutil
import sys

output=Path(sys.argv[1])/'licenses'
output.mkdir(parents=True,exist_ok=True)
for name in ('cryptography','qrcode','cffi','pycparser','pyinstaller','pywin32-ctypes','altgraph','pefile','packaging'):
    distribution=metadata.distribution(name)
    for file in distribution.files or []:
        if file.name.lower().startswith(('license','copying','notice')):
            source=Path(distribution.locate_file(file))
            if source.is_file():
                target=output/name/file.name
                target.parent.mkdir(parents=True,exist_ok=True)
                shutil.copyfile(source,target)
for filename in ('LICENSE.txt','LICENSE'):
    source=Path(sys.base_prefix)/filename
    if source.is_file():
        shutil.copyfile(source,output/'Python-LICENSE.txt')
        break
print('Runtime notices copied')
