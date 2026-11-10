public class Main {
    static class Counter {
        private long value;

        synchronized void inc() {
            value++;
        }

        synchronized long get() {
            return value;
        }
    }

    static class Worker extends Thread {
        final Counter counter;
        final int iterations;

        Worker(Counter counter, int iterations) {
            this.counter = counter;
            this.iterations = iterations;
        }

        @Override
        public void run() {
            for (int i = 0; i < iterations; i++) {
                counter.inc();
            }
        }
    }

    public static void main(String[] args) throws InterruptedException {
        Counter counter = new Counter();
        int n = 4;
        int per = 25000;
        Worker[] workers = new Worker[n];
        for (int i = 0; i < n; i++) {
            workers[i] = new Worker(counter, per);
        }
        for (Worker w : workers) {
            w.start();
        }
        for (Worker w : workers) {
            w.join();
        }
        System.out.println("threads=" + n);
        System.out.println("perThread=" + per);
        System.out.println("total=" + counter.get());
        System.out.println("expected=" + ((long) n * per));
        System.out.println("matched=" + (counter.get() == (long) n * per));
        Object lock = new Object();
        int[] shared = {0};
        Thread t1 = new Thread(() -> {
            synchronized (lock) {
                shared[0] = shared[0] + 1;
            }
        });
        t1.start();
        t1.join();
        System.out.println("shared=" + shared[0]);
        System.out.println("mainThread=" + Thread.currentThread().getName());
        System.out.println("t1.alive=" + t1.isAlive());
        Thread sleeper = new Thread(() -> {
            try {
                Thread.sleep(5);
            } catch (InterruptedException e) {
                System.out.println("interrupted");
            }
        });
        sleeper.start();
        sleeper.join();
        System.out.println("sleeper.alive=" + sleeper.isAlive());
        System.out.println("done");
    }
}
