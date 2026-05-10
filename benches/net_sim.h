#ifndef NET_SIM_H
#define NET_SIM_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

// Opaque handle to network simulator
typedef struct net_sim net_sim_t;

// Create a network simulator.
// lostrate: packet loss rate in permille (e.g. 100 = 10% loss)
// rtt_min: minimum one-way delay in milliseconds
// rtt_max: maximum one-way delay in milliseconds
// max_packets: maximum number of in-flight packets before dropping
net_sim_t* net_sim_create(int lostrate, int rtt_min, int rtt_max, int max_packets);

// Destroy simulator and free all queued packets
void net_sim_destroy(net_sim_t *sim);

// Send a packet through the simulator.
// peer: 0 or 1, identifies which direction
// data: packet data
// len: data length
// current: current timestamp in milliseconds
void net_sim_send(net_sim_t *sim, int peer, const char *data, int len, uint32_t current);

// Receive a packet from the simulator (if its delay has expired).
// peer: 0 or 1 (receives from the opposite direction of send)
// buf: output buffer
// maxsize: buffer capacity
// current: current timestamp in milliseconds
// Returns: number of bytes received, or -1 if no packet available
int net_sim_recv(net_sim_t *sim, int peer, char *buf, int maxsize, uint32_t current);

// Get statistics
int net_sim_tx1(const net_sim_t *sim);  // packets sent from peer 0
int net_sim_tx2(const net_sim_t *sim);  // packets sent from peer 1

#ifdef __cplusplus
}
#endif

#endif // NET_SIM_H
