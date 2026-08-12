/* bs_worker.c — shared one-shot background jobs (see bs_worker.h).
 *
 * All jobs run SERIALLY on ONE persistent worker thread: submit()
 * pushes the job on a small queue and signals a condvar; the worker
 * pops jobs one at a time and runs each fn; fn stores done=1
 * (release) as its last action.  One shared weak timer 'wkr' polls
 * the in-flight list every 100ms; a finished job's done_cb runs on
 * the main thread (the tick after fn set done).  The timer is
 * re-armed while any job is still in flight and stays disarmed
 * otherwise.
 *
 * The persistent thread is deliberate: a fresh pthread per job makes
 * glibc retain one thread stack and malloc arena per job (its stack
 * cache and arena list do not return that memory promptly), so a
 * 200-round sync would grow guest RSS by tens of MB.  One thread
 * means one stack and one arena; per-job allocations are freed by
 * the main thread's done_cb back into the same arena and get reused
 * by the next job.  No caller relies on jobs running in parallel:
 * sync rounds, downloads, and cover fetches are all one-at-a-time by
 * design (each domain guards its own in-flight job).
 *
 * The internal in-flight list is touched only on the main thread
 * (submit, tick, cancel_all); the pending queue is guarded by the
 * mutex; fn touches only its own job's fields (done/cancel/rc/
 * result/arg). */

#include "bookshelf.h"
#include "bs_worker.h"

#include <pthread.h>

static BsJob *g_jobs; /* in-flight list, main thread only */

/* Pending queue + the single worker thread. */
static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t  g_cv = PTHREAD_COND_INITIALIZER;
static BsJob          *g_queue;
static BsJob          *g_queue_tail;
static _Atomic int     g_worker_started;

/* One-shot 'wkr' timer callback. */
static void
bs_worker_timer_cb(void *ctx)
{
    (void)ctx;
    bs_worker_tick();
}

/* Persistent worker: pop jobs one at a time and run them. */
static void *
bs_worker_main(void *unused)
{
    (void)unused;
    for (;;) {
        pthread_mutex_lock(&g_mu);
        while (g_queue == NULL)
            pthread_cond_wait(&g_cv, &g_mu);
        BsJob *job = g_queue;
        g_queue = job->qnext;
        if (g_queue == NULL)
            g_queue_tail = NULL;
        pthread_mutex_unlock(&g_mu);
        job->fn(job); /* must store done=1 (release) as its last action */
    }
    return NULL; /* never */
}

/* Spawn the worker thread once.  On failure, fail every queued job
 * (the 'wkr' tick will run their done_cbs with rc=-1) and keep trying
 * on the next submit. */
static void
bs_worker_ensure(void)
{
    if (__atomic_load_n(&g_worker_started, __ATOMIC_ACQUIRE))
        return;
    pthread_mutex_lock(&g_mu);
    if (!__atomic_load_n(&g_worker_started, __ATOMIC_ACQUIRE)) {
        pthread_t t;
        if (pthread_create(&t, NULL, bs_worker_main, NULL) == 0) {
            pthread_detach(t);
            __atomic_store_n(&g_worker_started, 1, __ATOMIC_RELEASE);
        } else {
            while (g_queue != NULL) {
                BsJob *job = g_queue;
                g_queue = job->qnext;
                job->rc = -1;
                __atomic_store_n(&job->done, 1, __ATOMIC_RELEASE);
            }
            g_queue_tail = NULL;
        }
    }
    pthread_mutex_unlock(&g_mu);
}

BsJob *
bs_worker_submit(bs_job_fn fn, bs_job_done done_cb, void *arg)
{
    BsJob *job = calloc(1, sizeof *job);
    if (job == NULL)
        return NULL;
    job->fn = fn;
    job->done_cb = done_cb;
    job->arg = arg;
    job->next = g_jobs;
    g_jobs = job;

    pthread_mutex_lock(&g_mu);
    job->qnext = NULL;
    if (g_queue_tail)
        g_queue_tail->qnext = job;
    else
        g_queue = job;
    g_queue_tail = job;
    pthread_cond_signal(&g_cv);
    pthread_mutex_unlock(&g_mu);

    bs_worker_ensure();
    SetWeakTimerEx("wkr", bs_worker_timer_cb, NULL, 30);
    return job;
}

void
bs_worker_cancel(BsJob *job)
{
    if (job != NULL)
        __atomic_store_n(&job->cancel, 1, __ATOMIC_RELEASE);
}

void
bs_worker_cancel_all(void)
{
    for (BsJob *j = g_jobs; j != NULL; j = j->next)
        __atomic_store_n(&j->cancel, 1, __ATOMIC_RELEASE);
}

/* Main thread: run the done_cb of every finished job and free the job
 * struct (done_cb frees result/arg).  Re-arms 'wkr' while any job
 * remains in g_jobs, done or not: a done flag that flips between the
 * drain check and a busy() re-check would otherwise leave a finished
 * job unprocessed with the timer disarmed forever.  The next tick
 * settles it (and re-arms again while jobs are still running). */
void
bs_worker_tick(void)
{
    BsJob **pp = &g_jobs;
    while (*pp != NULL) {
        BsJob *job = *pp;
        if (__atomic_load_n(&job->done, __ATOMIC_ACQUIRE)) {
            *pp = job->next; /* unlink */
            job->done_cb(job);
            free(job);
        } else {
            pp = &job->next;
        }
    }
    if (g_jobs != NULL)
        SetWeakTimerEx("wkr", bs_worker_timer_cb, NULL, 30);
}
