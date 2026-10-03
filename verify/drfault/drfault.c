/* drfault: 실행 파일을 고치지 않고, 지정한 명령의 결과 레지스터(lane 0) 비트 하나를
 * 고른 스레드에서 N번째 실행마다(-every) 또는 결과가 특정 값일 때마다(-match), -after 이후에만
 * 뒤집는 DynamoRIO 클라이언트 — 한 코어만 틀리는 CPU 흉내.
 *
 * 옵션: -ops <이름,이름…> (-every <N> | -match <K>) [-after <N>] -bit <B> -thread worker|main|main+worker [-mask0] -log <경로>
 *   every 0 이면 주입하지 않는다(대조군). -match K(0~4095) = 결과 lane 0 하위 12비트가 K 일 때마다(같은 입력이면 늘 같이 틀림).
 *   -after N = 그 스레드에서 대상 명령을 N번 넘게 실행한 뒤부터만 주입(예: 정답표 자체 점검 구간을 건너뛰기).
 *   worker = 주 스레드가 아닌 스레드 중 처음으로 조건에 도달한 스레드 하나만, main = 처음 시작한 스레드만, main+worker = 둘 다.
 *   -mask0 = 레지스터 읽기·쓰기 왕복은 그대로 하되 0 을 XOR (왕복 자체가 상태를 깨지 않는지 보는 대조군).
 * 로그: 주입마다 {"tid","seq","cpu","op","count","bit","ms"} 한 줄, 끝에 {"exit_ms"} 한 줄
 *   seq = 스레드 시작 순서(주 스레드 0), cpu = rdtscp 보조값 하위 12비트(리눅스가 CPU 번호를 넣음, 고정되지 않은 스레드라 참고용),
 *   ms = 1601 기준 UTC 밀리초.
 * 리눅스 전용: 윈도우 x64 DynamoRIO 는 ymm6~15 를 보존하지 않는다(dr_mcontext_t 에 [xyz]mm0~5 자리만 있음).
 */
#include "dr_api.h"
#include "drmgr.h"
#include <x86intrin.h>
#include <stdlib.h>
#include <string.h>

enum { THREAD_WORKER, THREAD_MAIN, THREAD_MAIN_WORKER };

static bool target_op[OP_LAST + 1];
static uint64 every;
/* -match 값 (-1 = 안 씀) */
static int64 match = -1;
/* 스레드별 실행 수가 이 값을 넘은 뒤부터만 주입 */
static uint64 after;
static uint bit;
static bool mask0;
static int thread_mode = THREAD_WORKER;
static file_t log_file = INVALID_FILE;
static void *log_lock;
static int tls_idx;
static thread_id_t main_tid;
/* worker·main+worker 모드에서 "불량 코어"로 고정된 일꾼 스레드 (0 = 아직 없음) */
static volatile int64 chosen_tid;
/* 스레드 시작 순서 */
static volatile int next_seq;

typedef struct {
    uint64 count;
    int seq;
} per_thread_t;

static void
die(const char *msg, const char *arg)
{
    dr_fprintf(STDERR, "drfault: %s %s\n", msg, arg == NULL ? "" : arg);
    dr_abort_with_code(2);
}

/* "vfmadd231pd,vpmuludq" → 해당 명령 번호들에 표시. 모르는 이름이면 멈춘다 */
static void
mark_ops(const char *list)
{
    char buf[512], *ctx = NULL;
    if (strlen(list) >= sizeof(buf))
        die("-ops 가 너무 김", NULL);
    strcpy(buf, list);
    for (char *name = strtok_r(buf, ",", &ctx); name != NULL; name = strtok_r(NULL, ",", &ctx)) {
        bool found = false;
        for (int op = OP_FIRST; op <= OP_LAST; op++) {
            if (strcmp(decode_opcode_name(op), name) == 0) {
                target_op[op] = true;
                found = true;
            }
        }
        if (!found)
            die("모르는 명령 이름:", name);
    }
}

static void
parse(int argc, const char *argv[])
{
    const char *ops = NULL, *log = NULL;
    for (int i = 1; i < argc; i += 2) {
        const char *k = argv[i], *v = i + 1 < argc ? argv[i + 1] : NULL;
        if (strcmp(k, "-mask0") == 0) {
            mask0 = true;
            i--;
            continue;
        }
        if (v == NULL)
            die("값 없음:", k);
        if (strcmp(k, "-ops") == 0)
            ops = v;
        else if (strcmp(k, "-every") == 0)
            every = strtoull(v, NULL, 10);
        else if (strcmp(k, "-match") == 0) {
            char *end;
            match = strtoll(v, &end, 10);
            if (end == v || *end != '\0' || match < 0 || match > 0xfff)
                die("-match 는 0~4095:", v);
        } else if (strcmp(k, "-after") == 0)
            after = strtoull(v, NULL, 10);
        else if (strcmp(k, "-bit") == 0)
            bit = (uint)strtoul(v, NULL, 10);
        else if (strcmp(k, "-thread") == 0) {
            if (strcmp(v, "worker") == 0)
                thread_mode = THREAD_WORKER;
            else if (strcmp(v, "main") == 0)
                thread_mode = THREAD_MAIN;
            else if (strcmp(v, "main+worker") == 0)
                thread_mode = THREAD_MAIN_WORKER;
            else
                die("-thread 는 worker|main|main+worker:", v);
        } else if (strcmp(k, "-log") == 0)
            log = v;
        else
            die("모르는 옵션:", k);
    }
    if (ops == NULL || log == NULL)
        die("-ops 와 -log 필요", NULL);
    if (bit > 63)
        die("-bit 는 0~63", NULL);
    if (every > 0 && match >= 0)
        die("-every 와 -match 는 함께 못 씀", NULL);
    mark_ops(ops);
    log_file = dr_open_file(log, DR_FILE_WRITE_OVERWRITE);
    if (log_file == INVALID_FILE)
        die("로그 파일 열기 실패:", log);
}

/* 대상 명령 바로 뒤에서 불린다: 조건이 맞으면 목적 레지스터 lane 0 의 비트를 뒤집는다 */
static void
at_target(int opc, int reg)
{
    void *dc = dr_get_current_drcontext();
    thread_id_t tid = dr_get_thread_id(dc);
    bool is_main = tid == main_tid;
    if ((thread_mode == THREAD_MAIN && !is_main) || (thread_mode == THREAD_WORKER && is_main))
        return;
    /* 일꾼이 이미 정해졌으면 다른 일꾼은 바로 돌아간다 */
    if (!is_main && chosen_tid != 0 && chosen_tid != (int64)tid)
        return;
    per_thread_t *pt = (per_thread_t *)drmgr_get_tls_field(dc, tls_idx);
    pt->count++;
    if (pt->count <= after)
        return;
    if (match < 0 && (every == 0 || pt->count % every != 0))
        return;
    dr_mcontext_t mc = { 0 };
    mc.size = sizeof(mc);
    mc.flags = DR_MC_ALL;
    byte val[sizeof(dr_zmm_t)];
    uint64 lane0;
    if (!dr_get_mcontext(dc, &mc) || !reg_get_value_ex((reg_id_t)reg, &mc, val))
        die("레지스터 읽기 실패:", get_register_name((reg_id_t)reg));
    memcpy(&lane0, val, sizeof(lane0));
    if (match >= 0 && (int64)(lane0 & 0xfff) != match)
        return;
    if (!is_main) {
        /* 처음 도달한 일꾼 하나만 고정 — 그 뒤로는 그 스레드에서만 주입 */
        __sync_val_compare_and_swap(&chosen_tid, 0, (int64)tid);
        if (chosen_tid != (int64)tid)
            return;
    }
    lane0 ^= mask0 ? 0 : 1ULL << bit;
    memcpy(val, &lane0, sizeof(lane0));
    if (!reg_set_value_ex((reg_id_t)reg, &mc, val) || !dr_set_mcontext(dc, &mc))
        die("레지스터 쓰기 실패:", get_register_name((reg_id_t)reg));
    /* 클린 콜 안에서 libc 의 sched_getcpu 를 부르면 죽었다(CI 확인) — 명령으로 직접 읽는다 */
    unsigned int aux;
    __rdtscp(&aux);
    dr_mutex_lock(log_lock);
    dr_fprintf(log_file,
               "{\"tid\":" UINT64_FORMAT_STRING ",\"seq\":%d,\"cpu\":%d,\"op\":\"%s\",\"count\":" UINT64_FORMAT_STRING
               ",\"bit\":%u,\"ms\":" UINT64_FORMAT_STRING "}\n",
               (uint64)tid, pt->seq, (int)(aux & 0xfff), decode_opcode_name(opc), pt->count, bit, dr_get_milliseconds());
    dr_mutex_unlock(log_lock);
}

/* 모든 블록에서 대상 명령마다 그 바로 뒤에 클린 콜을 넣는다 (명령 뒤에 넣을 수 있는 마지막 단계 사용) */
static dr_emit_flags_t
event_bb(void *dc, void *tag, instrlist_t *bb, bool for_trace, bool translating)
{
    for (instr_t *in = instrlist_first_app(bb); in != NULL; in = instr_get_next_app(in)) {
        int opc = instr_get_opcode(in);
        if (opc < OP_FIRST || opc > OP_LAST || !target_op[opc] || instr_num_dsts(in) == 0 ||
            !opnd_is_reg(instr_get_dst(in, 0)))
            continue;
        /* 다음 명령 앞 = 이 명령 바로 뒤 (블록 끝이면 NULL → 맨 뒤에 붙는다) */
        dr_insert_clean_call_ex(dc, bb, instr_get_next(in), (void *)at_target,
                                DR_CLEANCALL_READS_APP_CONTEXT | DR_CLEANCALL_WRITES_APP_CONTEXT, 2,
                                OPND_CREATE_INT32(opc), OPND_CREATE_INT32(opnd_get_reg(instr_get_dst(in, 0))));
    }
    return DR_EMIT_DEFAULT;
}

static void
event_thread_init(void *dc)
{
    per_thread_t *pt = (per_thread_t *)dr_thread_alloc(dc, sizeof(*pt));
    pt->count = 0;
    pt->seq = dr_atomic_add32_return_sum(&next_seq, 1) - 1;
    drmgr_set_tls_field(dc, tls_idx, pt);
    /* 처음 시작한 스레드 = 주 스레드 */
    if (main_tid == 0)
        main_tid = dr_get_thread_id(dc);
}

static void
event_thread_exit(void *dc)
{
    dr_thread_free(dc, drmgr_get_tls_field(dc, tls_idx), sizeof(per_thread_t));
}

static void
event_exit(void)
{
    dr_fprintf(log_file, "{\"exit_ms\":" UINT64_FORMAT_STRING "}\n", dr_get_milliseconds());
    dr_close_file(log_file);
    dr_mutex_destroy(log_lock);
    drmgr_unregister_tls_field(tls_idx);
    drmgr_exit();
}

DR_EXPORT void
dr_client_main(client_id_t id, int argc, const char *argv[])
{
    dr_set_client_name("drfault", "");
    parse(argc, argv);
    if (!drmgr_init())
        die("drmgr_init 실패", NULL);
    log_lock = dr_mutex_create();
    tls_idx = drmgr_register_tls_field();
    if (tls_idx < 0 || !drmgr_register_thread_init_event(event_thread_init) ||
        !drmgr_register_thread_exit_event(event_thread_exit) ||
        !drmgr_register_bb_instru2instru_event(event_bb, NULL))
        die("이벤트 등록 실패", NULL);
    dr_register_exit_event(event_exit);
}
