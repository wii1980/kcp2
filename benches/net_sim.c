#include "net_sim.h"
#include <stdlib.h>
#include <string.h>

/* ---- xorshift32 PRNG (internal, reproducible) ---- */

static inline uint32_t xs32_next(uint32_t *state)
{
    uint32_t x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    return x;
}

/* ---- delayed-packet node with flexible array member ---- */

typedef struct delay_pkt {
    struct delay_pkt *next;
    struct delay_pkt *prev;
    uint32_t ts;     /* absolute timestamp when packet becomes available */
    int      len;    /* payload length */
    char     data[]; /* flexible array member (C99) */
} delay_pkt_t;

/* ---- simulator state ---- */

struct net_sim {
    /* Per-direction doubly-linked lists */
    delay_pkt_t *p12_head;  /* packets 1→2 */
    delay_pkt_t *p12_tail;
    int          p12_count;

    delay_pkt_t *p21_head;  /* packets 2→1 */
    delay_pkt_t *p21_tail;
    int          p21_count;

    int lostrate;     /* permille loss */
    int rtt_min;      /* ms */
    int rtt_max;      /* ms */
    int max_packets;  /* max in-flight per direction */

    /* Stats */
    int tx1, tx2;

    /* Internal PRNG state */
    uint32_t prng_state;
};

/* ---- linked-list helpers ---- */

static void list_insert_sorted(delay_pkt_t **head, delay_pkt_t **tail,
                               int *count, delay_pkt_t *pkt)
{
    /* Traverse from tail backward — most packets arrive in order,
       so insertion near tail is O(1) amortised. */
    delay_pkt_t *cur = *tail;
    while (cur != NULL && cur->ts > pkt->ts) {
        cur = cur->prev;
    }

    if (cur == NULL) {
        pkt->next = *head;
        pkt->prev = NULL;
        if (*head)
            (*head)->prev = pkt;
        else
            *tail = pkt;
        *head = pkt;
    } else {
        /* Insert after cur */
        pkt->next = cur->next;
        pkt->prev = cur;
        cur->next = pkt;
        if (pkt->next)
            pkt->next->prev = pkt;
        else
            *tail = pkt;
    }
    (*count)++;
}

static delay_pkt_t *list_pop_head(delay_pkt_t **head, delay_pkt_t **tail,
                                  int *count)
{
    delay_pkt_t *pkt = *head;
    if (!pkt)
        return NULL;

    *head = pkt->next;
    if (pkt->next)
        pkt->next->prev = NULL;
    else
        *tail = NULL;

    pkt->next = NULL;
    pkt->prev = NULL;
    (*count)--;
    return pkt;
}

static void list_clear(delay_pkt_t **head, delay_pkt_t **tail, int *count)
{
    delay_pkt_t *cur = *head;
    while (cur) {
        delay_pkt_t *next = cur->next;
        free(cur);
        cur = next;
    }
    *head = *tail = NULL;
    *count = 0;
}

/* ---- public API ---- */

net_sim_t *net_sim_create(int lostrate, int rtt_min, int rtt_max, int max_packets)
{
    net_sim_t *sim = (net_sim_t *)calloc(1, sizeof(net_sim_t));
    if (!sim)
        return NULL;

    sim->lostrate    = lostrate;
    sim->rtt_min     = rtt_min;
    sim->rtt_max     = rtt_max;
    sim->max_packets = max_packets;

    sim->prng_state = 42;

    return sim;
}

void net_sim_destroy(net_sim_t *sim)
{
    if (!sim)
        return;

    list_clear(&sim->p12_head, &sim->p12_tail, &sim->p12_count);
    list_clear(&sim->p21_head, &sim->p21_tail, &sim->p21_count);
    free(sim);
}

void net_sim_send(net_sim_t *sim, int peer, const char *data, int len,
                  uint32_t current)
{
    if (!sim || !data || len <= 0)
        return;

    if (peer == 0)
        sim->tx1++;
    else
        sim->tx2++;

    if ((xs32_next(&sim->prng_state) % 1000) < (uint32_t)sim->lostrate)
        return;

    delay_pkt_t **head  = (peer == 0) ? &sim->p12_head : &sim->p21_head;
    delay_pkt_t **tail  = (peer == 0) ? &sim->p12_tail : &sim->p21_tail;
    int          *count = (peer == 0) ? &sim->p12_count : &sim->p21_count;

    if (*count >= sim->max_packets)
        return;

    uint32_t delay;
    if (sim->rtt_max > sim->rtt_min)
        delay = (uint32_t)sim->rtt_min +
                (xs32_next(&sim->prng_state) %
                 (uint32_t)(sim->rtt_max - sim->rtt_min));
    else
        delay = (uint32_t)sim->rtt_min;

    delay_pkt_t *pkt = (delay_pkt_t *)malloc(sizeof(delay_pkt_t) + (size_t)len);
    if (!pkt)
        return;

    pkt->ts  = current + delay;
    pkt->len = len;
    memcpy(pkt->data, data, (size_t)len);
    pkt->next = NULL;
    pkt->prev = NULL;

    list_insert_sorted(head, tail, count, pkt);
}

int net_sim_recv(net_sim_t *sim, int peer, char *buf, int maxsize,
                 uint32_t current)
{
    if (!sim || !buf || maxsize <= 0)
        return -1;

    delay_pkt_t **head  = (peer == 0) ? &sim->p21_head : &sim->p12_head;
    delay_pkt_t **tail  = (peer == 0) ? &sim->p21_tail : &sim->p12_tail;
    int          *count = (peer == 0) ? &sim->p21_count : &sim->p12_count;

    if (!*head)
        return -1;

    if ((*head)->ts > current)
        return -1;

    delay_pkt_t *pkt = list_pop_head(head, tail, count);

    int copy_len = pkt->len < maxsize ? pkt->len : maxsize;
    memcpy(buf, pkt->data, (size_t)copy_len);
    int ret = pkt->len;

    free(pkt);
    return ret;
}

int net_sim_tx1(const net_sim_t *sim)
{
    return sim ? sim->tx1 : 0;
}

int net_sim_tx2(const net_sim_t *sim)
{
    return sim ? sim->tx2 : 0;
}
