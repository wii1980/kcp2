CC = gcc
CFLAGS = -O3 -march=native -mtune=native -flto \
         -fno-plt -fomit-frame-pointer \
         -DNDEBUG -Wall -Wextra
LDFLAGS = -flto -lm
IKCP_DIR = ../kcp-c

.PHONY: all clean run

all: bench

bench: bench.c bench_util.c net_sim.c $(IKCP_DIR)/ikcp.c
	$(CC) $(CFLAGS) -o $@ bench.c bench_util.c net_sim.c $(IKCP_DIR)/ikcp.c \
		-I$(IKCP_DIR) -I. $(LDFLAGS)

run: bench
	./bench

# Run with performance tuning (requires root or appropriate capabilities)
run-fast: bench
	taskset -c 3 ./bench

clean:
	rm -f bench
