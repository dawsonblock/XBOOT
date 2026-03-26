#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#define SERIAL_DEV "/dev/ttyS0"
#define HEADER_BUF 512
#define MAX_REQUEST_ID 128
#define MAX_FRAME_HEX (256 * 1024)
#define MAX_CODE_BYTES (128 * 1024)
#define MAX_STDIN_BYTES (64 * 1024)
#define MAX_STDOUT_BYTES (64 * 1024)
#define MAX_STDERR_BYTES (64 * 1024)
#define FLAG_STDOUT_TRUNCATED 1
#define FLAG_STDERR_TRUNCATED 2
#define FLAG_RECYCLE_REQUESTED 4

static int serial_fd = -1;

typedef struct {
    const char *name;
    pid_t pid;
    int to_child;
    int from_child;
    int available;
} Worker;

static Worker python_worker = { .name = "python", .pid = -1, .to_child = -1, .from_child = -1, .available = 0 };
static Worker node_worker = { .name = "node", .pid = -1, .to_child = -1, .from_child = -1, .available = 0 };

static void serial_write_all(const void *buf, size_t len) {
    const uint8_t *p = (const uint8_t *)buf;
    while (len > 0) {
        ssize_t n = write(serial_fd, p, len);
        if (n < 0) {
            if (errno == EINTR) continue;
            return;
        }
        p += (size_t)n;
        len -= (size_t)n;
    }
}

static void serial_puts(const char *s) {
    serial_write_all(s, strlen(s));
}

static int read_exact_fd(int fd, void *buf, size_t len) {
    uint8_t *p = (uint8_t *)buf;
    while (len > 0) {
        ssize_t n = read(fd, p, len);
        if (n == 0) return -1;
        if (n < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        p += (size_t)n;
        len -= (size_t)n;
    }
    return 0;
}

static int read_line_fd(int fd, char *buf, size_t cap) {
    size_t i = 0;
    while (i + 1 < cap) {
        char c;
        ssize_t n = read(fd, &c, 1);
        if (n == 0) return -1;
        if (n < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (c == '\n') {
            buf[i] = 0;
            return 0;
        }
        if (c == '\r') continue;
        buf[i++] = c;
    }
    buf[cap - 1] = 0;
    return 0;
}

static uint32_t fnv1a32(const uint8_t *data, size_t len) {
    uint32_t hash = 0x811c9dc5u;
    size_t i;
    for (i = 0; i < len; i++) {
        hash ^= data[i];
        hash *= 0x01000193u;
    }
    return hash;
}

static char nibble_to_hex(uint8_t v) {
    return (v < 10) ? (char)('0' + v) : (char)('a' + (v - 10));
}

static int hex_to_nibble(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

static char *hex_encode_alloc(const uint8_t *data, size_t len) {
    char *out = (char *)malloc(len * 2 + 1);
    size_t i;
    if (!out) return NULL;
    for (i = 0; i < len; i++) {
        out[i * 2] = nibble_to_hex((data[i] >> 4) & 0x0f);
        out[i * 2 + 1] = nibble_to_hex(data[i] & 0x0f);
    }
    out[len * 2] = 0;
    return out;
}

static uint8_t *hex_decode_alloc(const char *hex, size_t hex_len, size_t *out_len) {
    size_t i;
    uint8_t *out;
    if ((hex_len % 2) != 0) return NULL;
    out = (uint8_t *)malloc(hex_len / 2);
    if (!out) return NULL;
    for (i = 0; i < hex_len; i += 2) {
        int hi = hex_to_nibble(hex[i]);
        int lo = hex_to_nibble(hex[i + 1]);
        if (hi < 0 || lo < 0) {
            free(out);
            return NULL;
        }
        out[i / 2] = (uint8_t)((hi << 4) | lo);
    }
    *out_len = hex_len / 2;
    return out;
}

static void truncate_buffer(uint8_t **buf, size_t *len, size_t max_len, int *flags, int bit) {
    if (*len <= max_len) return;
    *len = max_len;
    *flags |= bit;
}

static int write_all_fd(int fd, const void *buf, size_t len) {
    const uint8_t *p = (const uint8_t *)buf;
    while (len > 0) {
        ssize_t n = write(fd, p, len);
        if (n < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        p += (size_t)n;
        len -= (size_t)n;
    }
    return 0;
}

static void kill_worker(Worker *worker) {
    if (worker->pid > 0) {
        kill(worker->pid, SIGKILL);
        waitpid(worker->pid, NULL, 0);
    }
    if (worker->to_child >= 0) close(worker->to_child);
    if (worker->from_child >= 0) close(worker->from_child);
    worker->pid = -1;
    worker->to_child = -1;
    worker->from_child = -1;
    worker->available = 0;
}

static int start_worker(Worker *worker, char *const argv[]) {
    int to_child[2];
    int from_child[2];
    char ready[64];
    pid_t pid;

    if (pipe(to_child) != 0) return -1;
    if (pipe(from_child) != 0) {
        close(to_child[0]); close(to_child[1]);
        return -1;
    }

    pid = fork();
    if (pid < 0) {
        close(to_child[0]); close(to_child[1]);
        close(from_child[0]); close(from_child[1]);
        return -1;
    }
    if (pid == 0) {
        dup2(to_child[0], STDIN_FILENO);
        dup2(from_child[1], STDOUT_FILENO);
        dup2(from_child[1], STDERR_FILENO);
        close(to_child[0]); close(to_child[1]);
        close(from_child[0]); close(from_child[1]);
        execvp(argv[0], argv);
        _exit(127);
    }

    close(to_child[0]);
    close(from_child[1]);
    worker->pid = pid;
    worker->to_child = to_child[1];
    worker->from_child = from_child[0];
    worker->available = 0;

    if (read_line_fd(worker->from_child, ready, sizeof(ready)) != 0) {
        kill_worker(worker);
        return -1;
    }
    if (strcmp(ready, "READY") != 0) {
        kill_worker(worker);
        return -1;
    }
    worker->available = 1;
    return 0;
}

static int restart_worker(Worker *worker) {
    kill_worker(worker);
    if (strcmp(worker->name, "python") == 0) {
        char *const argv[] = { "python3", "/zeroboot/worker.py", NULL };
        return start_worker(worker, argv);
    }
    if (strcmp(worker->name, "node") == 0) {
        char *const argv[] = { "node", "/zeroboot/worker_node.js", NULL };
        return start_worker(worker, argv);
    }
    return -1;
}

static int send_worker_request(Worker *worker, const char *request_id, uint64_t timeout_ms, const uint8_t *code, size_t code_len, const uint8_t *stdin_buf, size_t stdin_len) {
    char header[HEADER_BUF];
    int n = snprintf(header, sizeof(header), "WRK1 %zu %llu %zu %zu\n", strlen(request_id), (unsigned long long)timeout_ms, code_len, stdin_len);
    if (n <= 0 || (size_t)n >= sizeof(header)) return -1;
    if (write_all_fd(worker->to_child, header, (size_t)n) != 0) return -1;
    if (write_all_fd(worker->to_child, request_id, strlen(request_id)) != 0) return -1;
    if (code_len > 0 && write_all_fd(worker->to_child, code, code_len) != 0) return -1;
    if (stdin_len > 0 && write_all_fd(worker->to_child, stdin_buf, stdin_len) != 0) return -1;
    return 0;
}

static int read_worker_response(Worker *worker, char *request_id, size_t request_id_cap, int *exit_code, char *error_type, size_t error_type_cap, uint8_t **stdout_buf, size_t *stdout_len, uint8_t **stderr_buf, size_t *stderr_len, int *flags) {
    char header[HEADER_BUF];
    size_t id_len = 0;
    size_t out_len = 0;
    size_t err_len = 0;
    if (read_line_fd(worker->from_child, header, sizeof(header)) != 0) return -1;
    if (sscanf(header, "WRK1R %zu %d %31s %zu %zu %d", &id_len, exit_code, error_type, &out_len, &err_len, flags) != 6) return -1;
    if (id_len + 1 > request_id_cap) return -1;
    *stdout_buf = (uint8_t *)malloc(out_len ? out_len : 1);
    *stderr_buf = (uint8_t *)malloc(err_len ? err_len : 1);
    if (!*stdout_buf || !*stderr_buf) return -1;
    if (read_exact_fd(worker->from_child, request_id, id_len) != 0) return -1;
    request_id[id_len] = 0;
    if (out_len > 0 && read_exact_fd(worker->from_child, *stdout_buf, out_len) != 0) return -1;
    if (err_len > 0 && read_exact_fd(worker->from_child, *stderr_buf, err_len) != 0) return -1;
    *stdout_len = out_len;
    *stderr_len = err_len;
    return 0;
}

static Worker *select_worker(const char *language) {
    if (strcmp(language, "node") == 0 || strcmp(language, "javascript") == 0) {
        return node_worker.available ? &node_worker : NULL;
    }
    return python_worker.available ? &python_worker : NULL;
}

static void send_serial_response(const char *request_id, int exit_code, const char *error_type, const uint8_t *stdout_buf, size_t stdout_len, const uint8_t *stderr_buf, size_t stderr_len, int flags) {
    char *stdout_hex = hex_encode_alloc(stdout_buf, stdout_len);
    char *stderr_hex = hex_encode_alloc(stderr_buf, stderr_len);
    char *body;
    char header[HEADER_BUF];
    size_t id_len = strlen(request_id);
    size_t stdout_hex_len = stdout_len * 2;
    size_t stderr_hex_len = stderr_len * 2;
    uint32_t checksum;
    size_t body_len;

    if (!stdout_hex || !stderr_hex) {
        serial_puts("ZB1R 5 -1 internal 0 0 0 deadbeef\nerror");
        free(stdout_hex); free(stderr_hex);
        return;
    }

    body_len = id_len + stdout_hex_len + stderr_hex_len;
    body = (char *)malloc(body_len ? body_len : 1);
    if (!body) {
        free(stdout_hex); free(stderr_hex);
        serial_puts("ZB1R 5 -1 internal 0 0 0 deadbeef\nerror");
        return;
    }
    memcpy(body, request_id, id_len);
    memcpy(body + id_len, stdout_hex, stdout_hex_len);
    memcpy(body + id_len + stdout_hex_len, stderr_hex, stderr_hex_len);
    checksum = fnv1a32((const uint8_t *)body, body_len);
    snprintf(header, sizeof(header), "ZB1R %zu %d %s %zu %zu %d %08x\n", id_len, exit_code, error_type, stdout_hex_len, stderr_hex_len, flags, checksum);
    serial_puts(header);
    serial_write_all(body, body_len);
    free(stdout_hex);
    free(stderr_hex);
    free(body);
}

static void send_protocol_error(const char *request_id, const char *message) {
    send_serial_response(request_id, -1, "protocol", (const uint8_t *)"", 0, (const uint8_t *)message, strlen(message), 0);
}

static int handle_request_frame(void) {
    char header[HEADER_BUF];
    size_t request_id_len = 0;
    char language[32];
    unsigned long long timeout_ms = 0;
    size_t code_hex_len = 0;
    size_t stdin_hex_len = 0;
    unsigned int checksum = 0;
    size_t body_len;
    char *body;
    char request_id[MAX_REQUEST_ID];
    uint8_t *code = NULL;
    uint8_t *stdin_buf = NULL;
    size_t code_len = 0;
    size_t stdin_len = 0;
    uint32_t got_checksum;
    Worker *worker;
    uint8_t *stdout_buf = NULL;
    uint8_t *stderr_buf = NULL;
    size_t stdout_len = 0;
    size_t stderr_len = 0;
    int exit_code = -1;
    int flags = 0;
    char error_type[32] = "internal";
    char worker_request_id[MAX_REQUEST_ID];

    if (read_line_fd(serial_fd, header, sizeof(header)) != 0) return -1;
    if (sscanf(header, "ZB1 %zu %31s %llu %zu %zu %x", &request_id_len, language, &timeout_ms, &code_hex_len, &stdin_hex_len, &checksum) != 6) {
        send_protocol_error("error", "invalid request header");
        return 0;
    }
    if (request_id_len == 0 || request_id_len >= MAX_REQUEST_ID) {
        send_protocol_error("error", "request id too large");
        return 0;
    }
    if (code_hex_len > MAX_FRAME_HEX || stdin_hex_len > MAX_FRAME_HEX) {
        send_protocol_error("error", "request body too large");
        return 0;
    }
    body_len = request_id_len + code_hex_len + stdin_hex_len;
    body = (char *)malloc(body_len ? body_len : 1);
    if (!body) {
        send_protocol_error("error", "allocation failure");
        return 0;
    }
    if (read_exact_fd(serial_fd, body, body_len) != 0) {
        free(body);
        return -1;
    }
    got_checksum = fnv1a32((const uint8_t *)body, body_len);
    if (got_checksum != checksum) {
        free(body);
        send_protocol_error("error", "checksum mismatch");
        return 0;
    }

    memcpy(request_id, body, request_id_len);
    request_id[request_id_len] = 0;
    code = hex_decode_alloc(body + request_id_len, code_hex_len, &code_len);
    stdin_buf = hex_decode_alloc(body + request_id_len + code_hex_len, stdin_hex_len, &stdin_len);
    free(body);
    if (!code || !stdin_buf) {
        free(code); free(stdin_buf);
        send_protocol_error(request_id, "invalid hex payload");
        return 0;
    }
    if (code_len > MAX_CODE_BYTES || stdin_len > MAX_STDIN_BYTES) {
        free(code); free(stdin_buf);
        send_protocol_error(request_id, "request exceeds guest limits");
        return 0;
    }

    worker = select_worker(language);
    if (!worker) {
        free(code); free(stdin_buf);
        send_serial_response(request_id, -1, "runtime", (const uint8_t *)"", 0, (const uint8_t *)"language runtime unavailable", 26, 0);
        return 0;
    }

    if (send_worker_request(worker, request_id, timeout_ms, code, code_len, stdin_buf, stdin_len) != 0 ||
        read_worker_response(worker, worker_request_id, sizeof(worker_request_id), &exit_code, error_type, sizeof(error_type), &stdout_buf, &stdout_len, &stderr_buf, &stderr_len, &flags) != 0) {
        restart_worker(worker);
        free(code); free(stdin_buf);
        free(stdout_buf); free(stderr_buf);
        send_serial_response(request_id, -1, "internal", (const uint8_t *)"", 0, (const uint8_t *)"worker restart required", 23, 0);
        return 0;
    }
    free(code); free(stdin_buf);

    if (strcmp(worker_request_id, request_id) != 0) {
        free(stdout_buf); free(stderr_buf);
        send_protocol_error(request_id, "mismatched worker response id");
        return 0;
    }

    truncate_buffer(&stdout_buf, &stdout_len, MAX_STDOUT_BYTES, &flags, FLAG_STDOUT_TRUNCATED);
    truncate_buffer(&stderr_buf, &stderr_len, MAX_STDERR_BYTES, &flags, FLAG_STDERR_TRUNCATED);
    send_serial_response(request_id, exit_code, error_type, stdout_buf ? stdout_buf : (const uint8_t *)"", stdout_len, stderr_buf ? stderr_buf : (const uint8_t *)"", stderr_len, flags);
    if ((flags & FLAG_RECYCLE_REQUESTED) != 0) restart_worker(worker);
    free(stdout_buf); free(stderr_buf);
    return 0;
}

int main(void) {
    mkdir("/proc", 0755);
    mkdir("/sys", 0755);
    mkdir("/dev", 0755);
    mount("proc", "/proc", "proc", 0, 0);
    mount("sysfs", "/sys", "sysfs", 0, 0);
    mount("devtmpfs", "/dev", "devtmpfs", 0, 0);

    serial_fd = open(SERIAL_DEV, O_RDWR | O_NOCTTY);
    if (serial_fd < 0) _exit(1);

    {
        char *const py_argv[] = { "python3", "/zeroboot/worker.py", NULL };
        char *const node_argv[] = { "node", "/zeroboot/worker_node.js", NULL };
        if (start_worker(&python_worker, py_argv) == 0) python_worker.available = 1;
        if (start_worker(&node_worker, node_argv) == 0) node_worker.available = 1;
    }

    {
        char ready_line[128];
        snprintf(ready_line, sizeof(ready_line), "ZEROBOOT_READY python=%d node=%d\n", python_worker.available, node_worker.available);
        serial_puts(ready_line);
    }

    while (1) {
        if (handle_request_frame() != 0) break;
    }

    kill_worker(&python_worker);
    kill_worker(&node_worker);
    return 0;
}
