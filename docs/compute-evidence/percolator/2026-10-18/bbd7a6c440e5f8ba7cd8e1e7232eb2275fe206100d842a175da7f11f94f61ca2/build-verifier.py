#!/usr/bin/env python3
"""Generate an isolated helper crate; only an explicit --build invokes Cargo."""
import argparse, json, pathlib, shutil, subprocess
p=argparse.ArgumentParser();p.add_argument('--checkout',type=pathlib.Path,required=True);p.add_argument('--project',type=pathlib.Path,required=True);p.add_argument('--build',action='store_true');a=p.parse_args()
root=a.checkout.resolve();project=a.project.resolve();project.mkdir(parents=True,exist_ok=True)
assert (root/'agent-os/crates/covenant-compute-protocol').is_dir()
toml='[package]\nname="verify-public-receipt"\nversion="0.1.0"\nedition="2021"\n[workspace]\n[dependencies]\nserde={version="1",features=["derive"]}\nserde_json="1"\n'
for name in ['covenant-compute-buyer','covenant-compute-protocol','covenant-mcp']:
 toml+=name+'={path='+json.dumps(str(root/'agent-os/crates'/name))+'}\n'
(project/'Cargo.toml').write_text(toml);(project/'src').mkdir(exist_ok=True);shutil.copyfile(pathlib.Path(__file__).with_name('verify.rs'),project/'src/main.rs')
if not (project/'Cargo.lock').exists():shutil.copyfile(root/'agent-os/Cargo.lock',project/'Cargo.lock')
if a.build:subprocess.run(['cargo','build','--manifest-path',str(project/'Cargo.toml')],check=True)
print(json.dumps({'project':str(project),'checkout_commit':subprocess.check_output(['git','-C',str(root),'rev-parse','HEAD'],text=True).strip(),'built':a.build}))
