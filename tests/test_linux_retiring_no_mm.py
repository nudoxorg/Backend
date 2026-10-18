"""A retiring singleton is visible and nonterminal; ambiguity still blocks census."""
import contextlib
import dataclasses
import errno
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

SCRIPT=Path(__file__).parents[1]/'.config/scripts/build-census.py'
SPEC=importlib.util.spec_from_file_location('retiring_no_mm_census',SCRIPT)
census=importlib.util.module_from_spec(SPEC);sys.modules[SPEC.name]=census;SPEC.loader.exec_module(census)
PID=4100
GOOD=census._LinuxProcStat(state='D',ppid=1,pgid=PID,session=PID,flags=census._LINUX_PF_EXITING,start_ticks='1234',num_threads=1,vsize=0,rss=0)
STATUS=f'Pid:\t{PID}\nTgid:\t{PID}\nThreads:\t1\nUid:\t0\t0\t0\t0\n'

class Proc:
    def __init__(self, *,stats=None,ready=None,statuses=None,tasks=None,exe_errno=errno.ENOENT):
        self.stats=list(stats or [GOOD]*3);self.ready=list([False]*4 if ready is None else ready)
        self.statuses=list(statuses or [STATUS]*2);self.tasks=list(tasks or [(PID,)]*2);self.exe_errno=exe_errno
        self.pid=PID;self.pidfd=17;self.pidfd_open_error=None;self.directory_open_error=None
    def __enter__(self):return self
    def __exit__(self,*_):return None
    def pin_pidfd(self):pass
    def read_stat(self):return self.stats.pop(0)
    def pidfd_ready(self):return self.ready.pop(0)
    def read_text(self,name,*_):
        assert name=='status';return self.statuses.pop(0)
    def readlink(self,name):
        assert name=='exe';raise OSError(self.exe_errno,'injected exe read error','exe')
    def task_members(self):
        value=self.tasks.pop(0)
        if isinstance(value,BaseException):raise value
        return value

class RetiringTests(unittest.TestCase):
    def row(self,comm='nix'):
        return census._PsRow(PID,1,PID,0,'actual ps display time',comm)
    def record(self,proc=None,comm='nix'):
        proc=proc or Proc()
        return census._linux_process_record(self.row(comm),'boot',census._ArgvBudget(),proc_factory=lambda _:proc)
    def scan(self,proc,comm='nix'):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);boot=root/'sys/kernel/random/boot_id';boot.parent.mkdir(parents=True);boot.write_text('boot')
            return census._linux_processes(0,[self.row(comm)],census._ArgvBudget(),proc_root=root,proc_factory=lambda _:proc)
    def test_proved_singleton_is_retained_nonterminal_and_consumes_no_compiler_slot(self):
        records,issues,races=self.scan(Proc());self.assertEqual(issues,{});self.assertEqual(races,{})
        record=records[PID];self.assertEqual(record.state,'D');self.assertTrue(record.identity_validated)
        self.assertEqual(record.start_token,'linux:boot:1234');self.assertIsNone(record.executable)
        proof=record.linux_retiring_no_mm;self.assertEqual(proof['classification'],'kernel-retiring-no-mm-nonterminal')
        self.assertFalse(proof['terminal']);self.assertFalse(proof['exit_or_reaping_proven'])
        snapshot=census.ProcessSnapshot(records,'start','end',True,{}, {},0,1,0,0,False)
        self.assertEqual(census.cargo_entries(snapshot),[]);self.assertEqual(census.compiler_groups(snapshot,[]),[])
        with patch.object(census,'_snapshot_now',return_value=snapshot),patch.object(census.sys,'platform','linux'):
            output=io.StringIO()
            with contextlib.redirect_stdout(output):exit_code=census.main([])
        report=json.loads(output.getvalue());self.assertEqual(exit_code,0)
        self.assertEqual(report['kernel_retiring_no_mm'],[proof]);self.assertTrue(report['complete'])
    def test_each_stat_gate_and_both_identity_ends_are_required(self):
        for change in [dict(state='S'),dict(state='R'),dict(flags=0),dict(num_threads=2),dict(vsize=1),dict(rss=1),dict(vsize=None),dict(rss=None)]:
            with self.subTest(change=change):
                bad=dataclasses.replace(GOOD,**change)
                records,issues,_=self.scan(Proc(stats=[bad]*3));self.assertEqual(records,{})
                self.assertTrue(issues)
        for position in range(3):
            for change in [dict(start_ticks='9999'),dict(flags=GOOD.flags|0x100),dict(state='R'),dict(num_threads=2),dict(vsize=1),dict(rss=1)]:
                with self.subTest(position=position,change=change):
                    stats=[GOOD]*3;stats[position]=dataclasses.replace(GOOD,**change)
                    records,issues,_=self.scan(Proc(stats=stats));self.assertEqual(records,{})
                    self.assertTrue(issues)
        self.assertFalse(census._linux_retiring_stat(dataclasses.replace(GOOD,flags=GOOD.flags|census._LINUX_PF_KTHREAD)))
    def test_singleton_status_and_task_membership_are_both_required_and_rechecked(self):
        for status in [STATUS.replace('Tgid:\t4100','Tgid:\t1'),STATUS.replace('Threads:\t1','Threads:\t2'),STATUS.replace('Pid:\t4100','Pid:\t9'),STATUS.replace('Uid:\t0\t0\t0\t0','Uid:\t1\t0\t0\t0'),STATUS.replace('Threads:\t1\n','')]:
            for position in [0,1]:
                with self.subTest(status=status,position=position):
                    statuses=[STATUS]*2;statuses[position]=status
                    records,issues,_=self.scan(Proc(statuses=statuses));self.assertEqual(records,{})
                    self.assertTrue(issues)
        for tasks in [[(PID,PID+1),(PID,)],[(PID,),(PID,PID+1)],[(PID+1,),(PID+1,)]]:
            records,issues,_=self.scan(Proc(tasks=tasks));self.assertEqual(records,{})
            self.assertTrue(issues)
    def test_pidfd_is_present_and_not_ready_at_each_boundary(self):
        for position in range(4):
            for state in [None,True]:
                with self.subTest(position=position,state=state):
                    ready=[False]*4;ready[position]=state
                    records,issues,_=self.scan(Proc(ready=ready));self.assertEqual(records,{})
                    self.assertTrue(issues)
    def test_only_exe_enoent_is_eligible_and_other_errors_remain_fail_closed(self):
        for error in [errno.ESRCH,errno.EACCES,errno.EPERM,errno.EIO]:
            with self.subTest(error=error):
                records,issues,_=self.scan(Proc(exe_errno=error));self.assertEqual(records,{})
                self.assertTrue(issues)
        for error in [PermissionError(errno.EACCES,'task denied'),FileNotFoundError(errno.ENOENT,'task vanished')]:
            records,issues,_=self.scan(Proc(tasks=[error]));self.assertEqual(records,{})
            self.assertTrue(issues)
    def test_build_and_runtime_looking_names_keep_unknown_occupancy(self):
        for name in ['cargo','rustc','backend-locald','locald','backend-desktop','cargo.exe','rustc.exe','backend-locald.exe']:
            with self.subTest(name=name):
                records,issues,_=self.scan(Proc(),name);self.assertEqual(issues,{})
                self.assertIsNone(records[PID].linux_retiring_no_mm)
                snapshot=census.ProcessSnapshot(records,'start','end',True,{}, {},0,1,0,0,False)
                self.assertTrue(census.compiler_groups(snapshot,census.cargo_entries(snapshot)))
    def test_stat_parser_extracts_exact_kernel_fields(self):
        fields=['D','1',str(PID),str(PID),'0','0','4']+['0']*10+['1','0','1234','0','0']
        self.assertEqual(census._parse_linux_proc_stat(f'{PID} (nix) '+' '.join(fields)),GOOD)
    def test_task_members_uses_held_proc_dir_and_refuses_extra_thread(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);task=root/'task';task.mkdir();(task/str(PID)).mkdir()
            proc=census._LinuxProcDirectory(PID);proc.directory_fd=os.open(root,os.O_RDONLY|os.O_DIRECTORY)
            try:
                self.assertEqual(proc.task_members(),(PID,));(task/str(PID+1)).mkdir()
                with self.assertRaises(census._LinuxProcRace):proc.task_members()
            finally:proc.close()

if __name__=='__main__':unittest.main()
