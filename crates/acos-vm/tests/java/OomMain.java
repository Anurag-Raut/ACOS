import java.util.ArrayList;

public class OomMain {
    public static void main(String[] args) {
        try {
            ArrayList<long[]> list = new ArrayList<>();
            while (true) {
                list.add(new long[1024]);
            }
        } catch (OutOfMemoryError e) {
            System.out.println("caught-oom");
        }
        System.out.println("done");
    }
}
