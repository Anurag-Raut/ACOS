public class BenchAlloc {
    static class Node {
        int v;
        Node next;
        Node(int v) { this.v = v; }
    }
    public static void main(String[] args) {
        long s = 0;
        for (int i = 0; i < 10000000; i++) {
            Node n = new Node(i);
            n.next = n;
            s += n.v;
        }
        System.out.println("checksum=" + s);
    }
}
