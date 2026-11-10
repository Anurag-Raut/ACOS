// Framework-shaped: a field-access-heavy object graph walk with a mix of
// int, long and reference field reads/writes (iget/iput/iget-object/...).
public class BenchFields {
    static class Node {
        int v;
        long tag;
        Node next;
        Node prev;
    }

    public static void main(String[] args) {
        int n = 100000;
        Node[] nodes = new Node[n];
        for (int i = 0; i < n; i++) {
            Node x = new Node();
            x.v = i;
            nodes[i] = x;
        }
        for (int i = 0; i < n; i++) {
            nodes[i].next = nodes[(i + 1) % n];
            nodes[i].prev = nodes[(i + n - 1) % n];
        }
        long s = 0;
        Node cur = nodes[0];
        for (int i = 0; i < 5000000; i++) {
            s += cur.v;
            cur.tag = s;
            cur = cur.next;
            if (cur == nodes[0]) {
                s += cur.prev.v;
            }
        }
        System.out.println("checksum=" + s);
    }
}
