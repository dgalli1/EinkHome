/* eh_worker.h — one-shot background jobs with main-thread completion.
 *
 * Submit from the main thread; fn runs on a detached pthread and may
 * do blocking I/O (HTTP, file writes); when the job finishes, done_cb
 * runs on the main event loop, driven by one shared 30ms weak timer
 * 'wkr'.  SQLite and libinkview are only ever touched from done_cb,
 * never from fn.
 *
 * Ownership: the helper allocates and frees the BsJob struct itself.
 * arg is caller-allocated and done_cb frees it; result is
 * worker-allocated and done_cb frees it.  done_cb may submit new jobs
 * (chaining).  fn must store job->done = 1 (release) as its LAST
 * action so the tick can hand the job over to done_cb. */

#ifndef EH_WORKER_H
#define EH_WORKER_H

typedef struct BsJob BsJob;
typedef void (*eh_job_fn)(BsJob *job);    /* worker thread */
typedef void (*eh_job_done)(BsJob *job);  /* main thread */

struct BsJob {
    _Atomic int done;    /* release/acquire handoff: 1 = fn finished */
    _Atomic int cancel;  /* cooperative cancellation flag */
    int rc;              /* worker outcome (0 ok, nonzero error) */
    void *result;        /* worker-allocated; done_cb frees it */
    void *arg;           /* caller-allocated; done_cb frees it */
    eh_job_fn fn;
    eh_job_done done_cb;
    struct BsJob *next;   /* in-flight list linkage (main thread only) */
    struct BsJob *qnext;  /* pending-queue linkage (worker-thread lock) */
};

/* Submit a one-shot job.  Returns NULL only on internal allocation
 * failure (the caller should fail its operation gracefully). */
BsJob *eh_worker_submit(eh_job_fn fn, eh_job_done done_cb, void *arg);
void eh_worker_cancel(BsJob *job); /* sets the cancel flag */
void eh_worker_cancel_all(void);   /* EVT_EXIT: cancel every job */
void eh_worker_tick(void);         /* main thread: poll + run done_cbs */

#endif /* EH_WORKER_H */
