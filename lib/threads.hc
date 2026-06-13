#ifndef _THREADS_HC
#define _THREADS_HC
// threads.hc — implementation (interface in threads.hh).

#include <threads.hh>
#include <stdatomic.hh>
#include <time.hh>
#include <unistd.hh>
#include <heap.hh>

public U0 MutexInit(Mutex *m)
{
  AtomicStore(&m->state, 0);
}
public U0 MutexLock(Mutex *m)
{
  I64 c = AtomicCas(&m->state, 0, 1);  // fast path: 0 -> 1
  if (c != 0) {
    if (c != 2) c = AtomicSwap(&m->state, 2);  // mark contended
    while (c != 0) {
      FutexWait(&m->state, 2);                 // sleep until woken
      c = AtomicSwap(&m->state, 2);            // re-acquire as contended
    }
  }
}
public I64 MutexTryLock(Mutex *m)
{
  return AtomicCas(&m->state, 0, 1) == 0;
}
public U0 MutexUnlock(Mutex *m)
{
  if (AtomicSwap(&m->state, 0) == 2)  // there were waiters: wake one
    FutexWake(&m->state, 1);
}
public I64 MutexTimedLock(Mutex *m, I64 ns)
{
  I64 c = AtomicCas(&m->state, 0, 1);  // fast path: 0 -> 1
  if (c == 0) return 1;
  I64 deadline = NanoNS() + ns;
  if (c != 2) c = AtomicSwap(&m->state, 2);  // mark contended
  while (c != 0) {
    I64 left = deadline - NanoNS();
    if (left <= 0) return 0;
    FutexWaitNs(&m->state, 2, left);
    c = AtomicSwap(&m->state, 2);
  }
  return 1;
}
public U0 CondInit(Cond *c)
{
  AtomicStore(&c->seq, 0);
}
public U0 CondWait(Cond *c, Mutex *m)
{
  I64 seq = AtomicLoad(&c->seq);
  MutexUnlock(m);
  FutexWait(&c->seq, seq);
  MutexLock(m);
}
public I64 CondTimedWait(Cond *c, Mutex *m, I64 ns)
{
  I64 seq = AtomicLoad(&c->seq);
  MutexUnlock(m);
  FutexWaitNs(&c->seq, seq, ns);
  MutexLock(m);
  return AtomicLoad(&c->seq) != seq;
}
public U0 CondSignal(Cond *c)
{
  AtomicAdd(&c->seq, 1);
  FutexWake(&c->seq, 1);
}
public U0 CondBroadcast(Cond *c)
{
  AtomicAdd(&c->seq, 1);
  FutexWake(&c->seq, 0x7FFFFFFF);  // wake all
}
public U0 RwLockInit(RwLock *rw)
{
  AtomicStore(&rw->state, 0);
}
public U0 RwLockRLock(RwLock *rw)
{
  while (1) {
    I64 s = AtomicLoad(&rw->state);
    if (s >= 0) {
      if (AtomicCas(&rw->state, s, s + 1) == s) return;  // joined the readers
    } else {
      FutexWait(&rw->state, s);                          // a writer holds; wait
    }
  }
}
public U0 RwLockRUnlock(RwLock *rw)
{
  if (AtomicAdd(&rw->state, -1) == 0)  // the last reader left
    FutexWake(&rw->state, 1);          // let a waiting writer in
}
public U0 RwLockWLock(RwLock *rw)
{
  while (AtomicCas(&rw->state, 0, -1) != 0)         // 0 -> -1 (exclusive)
    FutexWait(&rw->state, AtomicLoad(&rw->state));  // wait while not free
}
public U0 RwLockWUnlock(RwLock *rw)
{
  AtomicStore(&rw->state, 0);
  FutexWake(&rw->state, 0x7FFFFFFF);  // wake all (waiting readers + a writer)
}
public U0 OnceInit(Once *o)
{
  AtomicStore(&o->state, 0);
}
public U0 CallOnce(Once *o, U0 (*fn)())
{
  I64 c = AtomicCas(&o->state, 0, 1);  // race to be the runner: 0 -> 1
  if (c == 0) {
    fn();
    AtomicStore(&o->state, 2);         // publish "done"
    FutexWake(&o->state, 0x7FFFFFFF);  // release everyone who blocked
    return;
  }
  while (c != 2) {                     // someone else is running it: wait
    FutexWait(&o->state, 1);
    c = AtomicLoad(&o->state);
  }
}

// --- thread-local storage (C11 `tss_*`, without destructors) ------------------
//
// Keyed storage where each thread sees its own value: `key = TssCreate(); TssSet(key,
// v); TssGet(key)`. A mutex-protected list of `(tid, key, value)` nodes over `Gettid`
// — pure HolyC, so it works identically on every target (and trivially in the
// synchronous interpreter, where there is one tid). Unlike C11 there are no
// destructors: a thread's entries are simply abandoned when it exits (a bounded leak
// of one node per (thread, key) pair, in the same spirit as the unreclaimed
// freestanding thread stacks). An unset key reads as 0.

class TssNode { TssNode *next; I64 tid; I64 key; I64 val; };
TssNode *tss_head;
Mutex tss_mu;        // zero-initialized = unlocked
I64 tss_next_key;
public I64 TssCreate() { return AtomicAdd(&tss_next_key, 1); }
public U0 TssSet(I64 key, I64 val)
{
  I64 tid = Gettid();
  MutexLock(&tss_mu);
  TssNode *n = tss_head;
  while (n) {
    if (n->tid == tid && n->key == key) {
      n->val = val;
      MutexUnlock(&tss_mu);
      return;
    }
    n = n->next;
  }
  n = MAlloc(sizeof(TssNode));
  n->next = tss_head;
  n->tid = tid;
  n->key = key;
  n->val = val;
  tss_head = n;
  MutexUnlock(&tss_mu);
}
public I64 TssGet(I64 key)
{
  I64 tid = Gettid();
  MutexLock(&tss_mu);
  TssNode *n = tss_head;
  while (n) {
    if (n->tid == tid && n->key == key) {
      I64 v = n->val;
      MutexUnlock(&tss_mu);
      return v;
    }
    n = n->next;
  }
  MutexUnlock(&tss_mu);
  return 0;
}

#endif
