import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.HashMap;
import java.util.Map;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.ThreadFactory;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * Dependency-free downstream used by the Spire vs nginx benchmark.
 *
 * <p>Ten endpoints (/api/1 .. /api/10) answer with an identical 96-byte body so
 * both gateways forward exactly the same number of bytes. Every endpoint lives
 * in its own class on purpose: chaosblade injects latency per class+method, so
 * "exactly one of ten endpoints is slow" is one
 * {@code blade create jvm delay --classname Backend$E7 --methodname serve ...}
 * away.
 *
 * <p>Why not com.sun.net.httpserver: it writes headers and body in two separate
 * syscalls, which on loopback triggers Nagle plus delayed ACK (~40ms) and caps
 * the whole stack at a few hundred requests per second. That floor would swamp
 * any gateway comparison, so the response is written in a single syscall on a
 * socket with TCP_NODELAY set, and connections are kept alive and reused.
 *
 * <p>Sources stay Java 8 compatible (compiled with {@code --release 8}) because
 * the chaosblade 1.7.3 jvm plugin attaches far more reliably to an older JVM.
 *
 * <p>Environment: PORT (default 8080), THREADS (default 2000 connection threads).
 */
public final class Backend {
    /** Identical byte length for all endpoints: both gateways must see the same payload. */
    private static final int BODY_LENGTH = 96;

    private Backend() {}

    static byte[] body(int id) {
        String prefix = String.format("{\"id\":\"%02d\",\"payload\":\"", id);
        String suffix = "\",\"slow\":false}";
        int pad = BODY_LENGTH - prefix.length() - suffix.length();
        StringBuilder sb = new StringBuilder(BODY_LENGTH);
        sb.append(prefix);
        for (int i = 0; i < pad; i++) {
            sb.append('x');
        }
        sb.append(suffix);
        return sb.toString().getBytes(StandardCharsets.US_ASCII);
    }

    /** Pre-rendered full HTTP/1.1 response: one write, no header/body split. */
    static byte[] response(int id) {
        byte[] payload = body(id);
        String head =
                "HTTP/1.1 200 OK\r\n"
                        + "Content-Type: application/json\r\n"
                        + "Content-Length: "
                        + payload.length
                        + "\r\n"
                        + "Connection: keep-alive\r\n\r\n";
        byte[] headBytes = head.getBytes(StandardCharsets.US_ASCII);
        byte[] out = new byte[headBytes.length + payload.length];
        System.arraycopy(headBytes, 0, out, 0, headBytes.length);
        System.arraycopy(payload, 0, out, headBytes.length, payload.length);
        return out;
    }

    /** One keep-alive connection. */
    static final class Conn {
        private final Socket socket;
        private final InputStream in;
        private final OutputStream out;
        private final byte[] buffer = new byte[8192];
        private int filled;

        Conn(Socket socket) throws IOException {
            this.socket = socket;
            this.in = socket.getInputStream();
            this.out = socket.getOutputStream();
        }

        /** Reads one request head plus body and returns its path, or null on EOF. */
        String readPath() throws IOException {
            filled = 0;
            while (true) {
                int headEnd = indexOfHeaderEnd();
                if (headEnd >= 0) {
                    String head = new String(buffer, 0, headEnd, StandardCharsets.US_ASCII);
                    String path = parsePath(head);
                    int contentLength = parseContentLength(head);
                    int bodyStart = headEnd + 4;
                    while (contentLength > 0) {
                        int available = filled - bodyStart;
                        if (available >= contentLength) {
                            break;
                        }
                        if (filled == buffer.length) {
                            return null;
                        }
                        int read = in.read(buffer, filled, buffer.length - filled);
                        if (read < 0) {
                            return null;
                        }
                        filled += read;
                    }
                    return path;
                }
                if (filled == buffer.length) {
                    return null;
                }
                int read = in.read(buffer, filled, buffer.length - filled);
                if (read < 0) {
                    return null;
                }
                filled += read;
            }
        }

        void write(byte[] response) throws IOException {
            out.write(response);
            out.flush();
        }

        void close() {
            try {
                socket.close();
            } catch (IOException ignored) {
                // A closed connection needs no further cleanup.
            }
        }

        private int indexOfHeaderEnd() {
            for (int i = 0; i + 3 < filled; i++) {
                if (buffer[i] == '\r' && buffer[i + 1] == '\n' && buffer[i + 2] == '\r' && buffer[i + 3] == '\n') {
                    return i;
                }
            }
            return -1;
        }

        private String parsePath(String head) {
            int start = head.indexOf(' ');
            if (start < 0) {
                return null;
            }
            int end = head.indexOf(' ', start + 1);
            if (end < 0) {
                return null;
            }
            return head.substring(start + 1, end);
        }

        private int parseContentLength(String head) {
            String needle = "Content-Length:";
            int pos = head.indexOf(needle);
            if (pos < 0) {
                return 0;
            }
            int start = pos + needle.length();
            int end = head.indexOf('\r', start);
            try {
                return Integer.parseInt(head.substring(start, end).trim());
            } catch (RuntimeException ignored) {
                return 0;
            }
        }
    }

    /** Base type for the ten endpoints; chaosblade targets the concrete subclass. */
    abstract static class Endpoint {
        final byte[] response;

        Endpoint(int id) {
            this.response = response(id);
        }

        abstract void serve(Conn conn) throws IOException;
    }

    public static final class E1 extends Endpoint {
        public E1() {
            super(1);
        }

        @Override
        void serve(Conn conn) throws IOException {
            conn.write(response);
        }
    }

    public static final class E2 extends Endpoint {
        public E2() {
            super(2);
        }

        @Override
        void serve(Conn conn) throws IOException {
            conn.write(response);
        }
    }

    public static final class E3 extends Endpoint {
        public E3() {
            super(3);
        }

        @Override
        void serve(Conn conn) throws IOException {
            conn.write(response);
        }
    }

    public static final class E4 extends Endpoint {
        public E4() {
            super(4);
        }

        @Override
        void serve(Conn conn) throws IOException {
            conn.write(response);
        }
    }

    public static final class E5 extends Endpoint {
        public E5() {
            super(5);
        }

        @Override
        void serve(Conn conn) throws IOException {
            conn.write(response);
        }
    }

    public static final class E6 extends Endpoint {
        public E6() {
            super(6);
        }

        @Override
        void serve(Conn conn) throws IOException {
            conn.write(response);
        }
    }

    public static final class E7 extends Endpoint {
        public E7() {
            super(7);
        }

        @Override
        void serve(Conn conn) throws IOException {
            conn.write(response);
        }
    }

    public static final class E8 extends Endpoint {
        public E8() {
            super(8);
        }

        @Override
        void serve(Conn conn) throws IOException {
            conn.write(response);
        }
    }

    public static final class E9 extends Endpoint {
        public E9() {
            super(9);
        }

        @Override
        void serve(Conn conn) throws IOException {
            conn.write(response);
        }
    }

    public static final class E10 extends Endpoint {
        public E10() {
            super(10);
        }

        @Override
        void serve(Conn conn) throws IOException {
            conn.write(response);
        }
    }

    private static final class ConnThreads implements ThreadFactory {
        private final AtomicInteger seq = new AtomicInteger();

        @Override
        public Thread newThread(Runnable task) {
            Thread thread = new Thread(task, "conn-" + seq.incrementAndGet());
            thread.setDaemon(true);
            return thread;
        }
    }

    public static void main(String[] args) throws IOException {
        String portEnv = System.getenv("PORT");
        String threadsEnv = System.getenv("THREADS");
        int port = Integer.parseInt(portEnv == null ? "8080" : portEnv);
        int threads = Integer.parseInt(threadsEnv == null ? "2000" : threadsEnv);

        Map<String, Endpoint> routes = new HashMap<String, Endpoint>();
        routes.put("/health", null);
        routes.put("/api/1", new E1());
        routes.put("/api/2", new E2());
        routes.put("/api/3", new E3());
        routes.put("/api/4", new E4());
        routes.put("/api/5", new E5());
        routes.put("/api/6", new E6());
        routes.put("/api/7", new E7());
        routes.put("/api/8", new E8());
        routes.put("/api/9", new E9());
        routes.put("/api/10", new E10());

        final byte[] health = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok"
                .getBytes(StandardCharsets.US_ASCII);

        ExecutorService executor = Executors.newFixedThreadPool(threads, new ConnThreads());
        ServerSocket server = new ServerSocket();
        server.setReuseAddress(true);
        server.bind(new InetSocketAddress("0.0.0.0", port), 4096);
        System.out.printf(
                "backend listening on 0.0.0.0:%d (threads=%d, body=%d bytes)%n", port, threads, BODY_LENGTH);

        while (true) {
            final Socket socket = server.accept();
            executor.execute(
                    new Runnable() {
                        @Override
                        public void run() {
                            Conn conn = null;
                            try {
                                socket.setTcpNoDelay(true);
                                socket.setSoTimeout(60000);
                                conn = new Conn(socket);
                                while (true) {
                                    String path = conn.readPath();
                                    if (path == null) {
                                        return;
                                    }
                                    if ("/health".equals(path)) {
                                        conn.write(health);
                                        continue;
                                    }
                                    Endpoint endpoint = routes.get(path);
                                    if (endpoint == null) {
                                        conn.write(
                                                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"
                                                        .getBytes(StandardCharsets.US_ASCII));
                                        continue;
                                    }
                                    endpoint.serve(conn);
                                }
                            } catch (IOException ignored) {
                                // Client went away: nothing to report for a benchmark target.
                            } finally {
                                if (conn != null) {
                                    conn.close();
                                }
                            }
                        }
                    });
        }
    }
}
