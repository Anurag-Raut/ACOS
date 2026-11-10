import java.util.regex.Matcher;
import java.util.regex.Pattern;

public class Main {
    static void p(String key, Object value) {
        System.out.println(key + "=" + value);
    }

    public static void main(String[] args) {
        Pattern ab = Pattern.compile("a*b");
        p("matches.full", ab.matcher("aaab").matches());
        p("matches.partial", ab.matcher("aaabc").matches());
        p("static.matches", Pattern.matches("a*b", "aaaaab"));
        p("static.matches.no", Pattern.matches("a*b", "xaaab"));

        Matcher m = Pattern.compile("(\\d+)-(\\d+)").matcher("12-34 56-78");
        p("groupCount", m.groupCount());
        while (m.find()) {
            p("find", m.group() + " " + m.start() + " " + m.end());
            p("groups", m.group(1) + "," + m.group(2));
            p("g1.bounds", m.start(1) + ":" + m.end(1));
        }
        p("find.after", m.find());

        Matcher look = Pattern.compile("\\w+").matcher("  hello world  ");
        p("lookingAt", look.lookingAt());
        look.reset();
        p("lookingAt.reset", look.lookingAt());

        p("replaceAll", "a1b22c333".replaceAll("\\d+", "#"));
        p("replaceFirst", "a1b22c333".replaceFirst("\\d+", "#"));
        p("replaceAll.groups", "john smith".replaceAll("(\\w+) (\\w+)", "$2 $1"));

        String[] parts = "a,b,,c".split(",");
        p("split.len", parts.length);
        p("split", "[" + String.join("|", parts) + "]");
        String[] partsLimit = "a,b,c,d".split(",", 3);
        p("split.limit", "[" + String.join("|", partsLimit) + "]");
        p("str.matches", "abc123".matches("[a-z]+\\d+"));
    }
}
