import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;

public class Main {
    static class Box<T> {
        private T value;

        Box(T value) {
            this.value = value;
        }

        T get() {
            return value;
        }

        void set(T value) {
            this.value = value;
        }

        @Override
        public String toString() {
            return "Box(" + value + ")";
        }
    }

    static <T extends Comparable<T>> T max(List<T> list) {
        T best = list.get(0);
        for (T t : list) {
            if (t.compareTo(best) > 0) best = t;
        }
        return best;
    }

    public static void main(String[] args) {
        List<String> list = new ArrayList<>();
        list.add("banana");
        list.add("apple");
        list.add("cherry");
        list.add("apple");
        System.out.println("list=" + list);
        System.out.println("list.size=" + list.size());
        System.out.println("list.contains=" + list.contains("apple"));
        System.out.println("list.indexOf=" + list.indexOf("apple"));
        System.out.println("list.lastIndexOf=" + list.lastIndexOf("apple"));
        System.out.println("list.get1=" + list.get(1));
        list.remove("apple");
        System.out.println("after.remove=" + list);
        Collections.sort(list);
        System.out.println("sorted=" + list);
        System.out.println("max=" + max(new ArrayList<>(Arrays.asList(3, 9, 2, 7))));
        Map<String, Integer> map = new TreeMap<>();
        map.put("delta", 4);
        map.put("alpha", 1);
        map.put("charlie", 3);
        map.put("bravo", 2);
        System.out.println("sorted.keys=" + map.keySet());
        StringBuilder sb = new StringBuilder("entries=");
        for (Map.Entry<String, Integer> e : map.entrySet()) {
            sb.append(e.getKey()).append('=').append(e.getValue()).append(' ');
        }
        System.out.println(sb);
        System.out.println("map.get=" + map.get("charlie"));
        System.out.println("map.containsKey=" + map.containsKey("echo"));
        map.put("alpha", 11);
        System.out.println("updated=" + map);
        HashMap<String, Integer> hash = new HashMap<>();
        hash.put("x", 1);
        System.out.println("hash.get=" + hash.get("x") + " size=" + hash.size());
        Integer a = 127, b = 127;
        Integer c = 128, d = 128;
        System.out.println("cache127=" + (a == b));
        System.out.println("cache128=" + (c == d));
        System.out.println("equals128=" + c.equals(d));
        Integer boxed = 1000;
        int unboxed = boxed;
        System.out.println("unboxed=" + unboxed);
        List<Integer> nums = new ArrayList<>();
        for (int i = 0; i < 5; i++) {
            nums.add(i * i);
        }
        System.out.println("nums=" + nums);
        System.out.println("box=" + new Box<>("hi") + " " + new Box<>(42));
        List<String> empty = new ArrayList<>();
        System.out.println("empty=" + empty.isEmpty());
        System.out.println("toArray=" + Arrays.toString(list.toArray()));
    }
}
