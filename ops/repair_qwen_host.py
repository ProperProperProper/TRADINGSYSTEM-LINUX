#!/usr/bin/env python3
"""Run from the owner's ordinary host terminal, not a sandbox workaround.

Tests the installed model; optionally restarts existing services and builds a
separate native GGUF training tool. Never downloads/replaces a model or places
an order. Each run preserves its own logs and report.
"""
import argparse
import hashlib
import json
import os
import shutil
import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

PROJECT=Path(__file__).resolve().parents[1]
MODEL=Path('/home/patrick/models/Qwen2.5-7B-Instruct-Q4_K_M.gguf')
SERVER=Path('/home/patrick/llama-bin/build/bin/llama-server')
TRAIN_ROOT=Path('/home/patrick/.local/share/tradingsystem-training')
TRAIN_COMMIT='fed4890a4ccd775d5c04b921720eda947d990f7d'
TRAIN_TAG='v2026.10.31'

class Repair:
    def __init__(self,root=None):
        self.out=(root or PROJECT/'papercompmoomoo/data/host_repairs')/str(time.time_ns())
        self.out.mkdir(parents=True,exist_ok=False)
        self.steps=[]

    def run(self,args,timeout=60,env=None,cwd=None):
        index=len(self.steps);start=time.monotonic()
        out=self.out/f'{index:03d}-stdout.log';err=self.out/f'{index:03d}-stderr.log'
        step={'command':[str(a) for a in args],'stdout':str(out),'stderr':str(err),
              'cwd':str(cwd) if cwd is not None else None}
        self.steps.append(step)
        with out.open('x') as stdout,err.open('x') as stderr:
            try:
                result=subprocess.run([str(a) for a in args],stdout=stdout,stderr=stderr,
                    timeout=timeout,env=env,cwd=cwd)
                step.update(returncode=result.returncode,seconds=time.monotonic()-start)
            except subprocess.TimeoutExpired:
                step.update(returncode=None,seconds=time.monotonic()-start,error='timeout')
                raise RuntimeError('command timed out; see '+str(err))
        if result.returncode:raise RuntimeError('command failed; see '+str(err))
        return out.read_text()

    def host_access(self):
        if os.getuid()!=1000:raise RuntimeError('Run as patrick in the normal terminal, without sudo.')
        try:
            with socket.socket():pass
        except PermissionError:
            raise RuntimeError('This process is still sandboxed: socket creation is denied. Run bash ~/fix-qwen.sh in your normal desktop terminal.')
        self.run(['systemctl','--user','list-units','--type=service','--no-pager','--no-legend'])

    def inference(self,repair=False):
        if not MODEL.is_file() or not SERVER.is_file():raise RuntimeError('installed model or inference server missing; refusing replacement')
        env=dict(os.environ,LD_LIBRARY_PATH=str(SERVER.parent))
        self.run([SERVER,'--version'],env=env)
        def request(path,payload=None,timeout=5):
            req=urllib.request.Request('http://127.0.0.1:8000'+path,
                data=json.dumps(payload).encode() if payload else None,
                headers={'Content-Type':'application/json'})
            with urllib.request.urlopen(req,timeout=timeout) as r:return json.loads(r.read())
        health=None
        try:health=request('/health')
        except (OSError,ValueError):
            if not repair:raise RuntimeError('Qwen health unavailable; rerun with --repair-inference')
            self.run(['systemctl','--user','restart','local.qwen.service'],timeout=30)
            for _ in range(90):
                try:
                    health=request('/health');break
                except (OSError,ValueError):time.sleep(2)
            if health is None:raise RuntimeError('Qwen did not become healthy; inspect local.qwen.service logs')
        models=request('/v1/models')
        prompt={'model':'qwen2.5-7b-instruct','messages':[{'role':'user','content':'Return exactly the JSON object {"ok":true} and nothing else.'}],
            'temperature':0,'max_tokens':24,'stream':False}
        start=time.monotonic();reply=request('/v1/chat/completions',prompt,timeout=90)
        with (self.out/'inference-response.json').open('x') as f:json.dump(reply,f,indent=2)
        text=reply['choices'][0]['message']['content'].strip()
        if json.loads(text)!={'ok':True}:raise RuntimeError('Qwen response failed the structured-output smoke test')
        return {'health':health,'models':models,'smoke_seconds':time.monotonic()-start,'usage':reply.get('usage'),
            'installed_model':str(MODEL),'model_replaced':False}

    def install_training(self):
        if not shutil.which('git') or not shutil.which('g++'):raise RuntimeError('git and g++ are required; no root installation attempted')
        TRAIN_ROOT.mkdir(parents=True,exist_ok=True)
        tools=TRAIN_ROOT/'build-tools';python=tools/'bin/python'
        if not python.exists():self.run([sys.executable,'-m','venv',tools],timeout=60)
        # This installs build tools only, never another model or a training corpus.
        self.run([python,'-m','pip','install','--disable-pip-version-check','--use-deprecated=legacy-certs','cmake','ninja'],timeout=600)
        source=TRAIN_ROOT/'llama-tq'
        if not source.exists():
            self.run(['git','clone','--depth','1','--branch',TRAIN_TAG,
                'https://github.com/LL4nc33/llama-tq.git',source],timeout=300)
        head=self.run(['git','-C',source,'rev-parse','HEAD']).strip()
        if head!=TRAIN_COMMIT:raise RuntimeError('training source revision differs from pinned release; refusing to overwrite or checkout existing source')
        if self.run(['git','-C',source,'status','--porcelain']).strip():
            raise RuntimeError('training source has local modifications; preserving it and stopping')
        build=source/'build';cmake=tools/'bin/cmake'
        env=dict(os.environ,PATH=str(tools/'bin')+os.pathsep+os.environ.get('PATH',''))
        self.run([cmake,'-S',source,'-B',build,'-G','Ninja','-DGGML_CUDA=OFF',
            '-DGGML_VULKAN=OFF','-DGGML_NATIVE=ON','-DLLAMA_CURL=OFF',
            '-DLLAMA_BUILD_TESTS=OFF','-DLLAMA_BUILD_SERVER=OFF','-DLLAMA_BUILD_EXAMPLES=ON'],
            timeout=180,env=env)
        self.run([cmake,'--build',build,'--target','llama-finetune','--parallel','5'],timeout=1800,env=env)
        binary=build/'bin/llama-finetune'
        env=dict(env,LD_LIBRARY_PATH=str(binary.parent))
        help_text=self.run([binary,'--help'],env=env)
        for flag in ('--lora-train-target','--grad-checkpoint','--stop-after'):
            if flag not in help_text:raise RuntimeError('built backend lacks '+flag)
        return {'binary':str(binary),'source_commit':head,'native_training_backend_installed':True,
            'gradient_updates_run':False,'model_replaced':False,
            'next_required':'Validate masked online-decision examples and measure actual Qwen2.5 gradients, memory and inference before enabling learning.'}

    def restart_project(self):
        listing=self.run(['systemctl','--user','list-units','--type=service','--state=active','--no-legend','--plain'])
        units=[line.split()[0] for line in listing.splitlines() if line.split()]
        prefixes=('local.papercompmoomoo.','local.orb.','com.thomasai.','local.learning-supervisor','local.candle-health','local.qwen-learning-agent')
        selected=[u for u in units if u.endswith('.service') and u.startswith(prefixes)]
        # Restart only services already active: do not enable dormant trading jobs.
        for unit in sorted(selected,key=lambda u:(0 if 'converter' in u else 1,u)):
            self.run(['systemctl','--user','restart',unit],timeout=90)
            self.run(['systemctl','--user','is-active',unit],timeout=10)
        return {'restarted_active_services':selected,'dormant_services_started':False}

    def install_exit_worker(self):
        """Install the Codex-only paper-exit worker through the tracked installer."""
        python=PROJECT/'papercompmoomoo/.venv/bin/python'
        if not python.is_file():raise RuntimeError('papercompmoomoo virtualenv is missing')
        env=dict(os.environ,PYTHONPATH='src')
        self.run([python,'-m','papercompmoomoo.install','codex-exit'],timeout=120,env=env,
                 cwd=PROJECT/'papercompmoomoo')
        unit='local.papercompmoomoo.codex-exit.service'
        self.run(['systemctl','--user','is-active',unit],timeout=10)
        return {'unit':unit,'active':True,'exclusive_owner':'codex','paper_only':True}

def main():
    p=argparse.ArgumentParser();p.add_argument('--repair-inference',action='store_true');p.add_argument('--install-training',action='store_true');p.add_argument('--restart-project',action='store_true');p.add_argument('--install-exit-worker',action='store_true')
    a=p.parse_args();worker=Repair();result={'status':'running','model_replaced':False}
    print('Repair report directory:',worker.out,flush=True)
    try:
        worker.host_access()
        verify_qwen=a.repair_inference or not (a.install_training or a.restart_project or a.install_exit_worker)
        if verify_qwen:
            result['inference']=worker.inference(a.repair_inference)
            print('Existing Qwen inference verified for learning/review.',flush=True)
        else:
            result['inference']={'skipped':'Qwen inference is not required for Codex exit-worker installation or service restart.'}
        if a.install_training:
            result['training']=worker.install_training();print('Native GGUF training backend built and checked.',flush=True)
        if a.install_exit_worker:
            result['exit_worker']=worker.install_exit_worker();print('Codex-only paper-exit worker installed and active.',flush=True)
        if a.restart_project:
            result['services']=worker.restart_project()
        result['status']='host_checks_complete'
    except Exception as exc:
        result.update(status='blocked',error=str(exc));print('STOPPED:',exc,flush=True)
    finally:
        result['steps']=worker.steps
        with (worker.out/'report.json').open('x') as f:json.dump(result,f,indent=2)
        print('Report:',worker.out/'report.json',flush=True)
    return 0 if result['status']=='host_checks_complete' else 1

if __name__=='__main__':raise SystemExit(main())
