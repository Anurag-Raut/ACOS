import java.util.regex.Matcher;
import java.util.regex.Pattern;

public class Main {
    static void p(String key, Object value) {
        System.out.println(key + "=" + value);
    }

    public static void main(String[] args) {
        // Backreferences.
        Matcher d = Pattern.compile("(\\w+)\\s+\\1").matcher("hello hello world world");
        while (d.find()) {
            p("dup", d.group(1));
        }
        p("dup.none", Pattern.compile("(\\w+)\\s+\\1").matcher("hello world").find());

        // Named groups.
        Matcher n = Pattern.compile("(?<year>\\d{4})-(?<month>\\d{2})-(?<day>\\d{2})")
                .matcher("Date: 2024-06-15");
        p("named.find", n.find());
        p("named.year", n.group("year"));
        p("named.month", n.group("month"));
        p("named.day", n.group("day"));
        p("named.replace", "2024-06-15".replaceAll(
                "(?<y>\\d{4})-(?<m>\\d{2})-(?<d>\\d{2})", "${d}/${m}/${y}"));

        // Lookahead / lookbehind.
        p("lookahead", "foo1 bar2".replaceAll("\\w+(?=\\d)", "X"));
        p("neg.lookahead", "foo1 fooX".replaceAll("foo(?!\\d)", "Y"));
        p("lookbehind", "$100".replaceAll("(?<=\\$)\\d+", "N"));
        p("lookbehind2", "100 dollars".replaceAll("(?<=100) dollars", " USD"));
        p("neg.lookbehind", "ab cb".replaceAll("(?<!a)b", "Z"));

        // Lazy vs greedy vs possessive / atomic.
        p("greedy", "<a><b>".replaceAll("<.*>", "G"));
        p("lazy", "<a><b>".replaceAll("<.*?>", "L"));
        p("possessive", "aaaa".matches("a*+a"));
        p("possessive2", "aaaa".matches("a*+"));
        p("atomic", "aaaa".matches("(?>a*)a"));

        // Alternation, anchors, boundaries, \G.
        p("alt", "cat dog cow".replaceAll("cat|dog", "pet"));
        p("anchor.multiline", "ab\ncd".replaceAll("(?m)^", ">"));
        p("word.boundary", "cat catalog".replaceAll("\\bcat\\b", "DOG"));
        p("G", "a1b2c3".replaceAll("\\G\\w\\d", "#"));

        // Inline flags.
        p("caseflag", "Hello WORLD".replaceAll("(?i)world", "there"));
        p("dotall", "a\nb".matches("(?s)a.b"));
        p("comments", "abc".matches("(?x)a b c"));
        p("scoped", Pattern.compile("(?i:abc)def").matcher("ABCdef").matches());

        // Multiline line-start.
        Matcher ml = Pattern.compile("(?m)^\\w+").matcher("one\ntwo\nthree");
        while (ml.find()) {
            p("ml", ml.group());
        }
    }
}
