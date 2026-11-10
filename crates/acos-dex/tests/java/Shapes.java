interface Shape {
    double area();

    default String describe() {
        return "shape:" + area();
    }

    static Shape unit() {
        return () -> 1.0;
    }
}

abstract class BaseShape implements Shape {
    protected final String name;

    protected BaseShape(String name) {
        this.name = name;
    }

    public String name() {
        return name;
    }

    public String toString() {
        return name + "=" + area();
    }
}

class Circle extends BaseShape {
    private final double r;

    Circle(double r) {
        super("circle");
        this.r = r;
    }

    @Override
    public double area() {
        return Math.PI * r * r;
    }
}

class Rect extends BaseShape {
    private final double w;
    private final double h;

    Rect(double w, double h) {
        super("rect");
        this.w = w;
        this.h = h;
    }

    @Override
    public double area() {
        return w * h;
    }
}

public class Shapes {
    public static double total(Shape[] shapes) {
        double sum = 0;
        for (Shape s : shapes) {
            sum += s.area();
            if (s instanceof BaseShape) {
                BaseShape b = (BaseShape) s;
                if (b.name().startsWith("c")) {
                    sum += 0.5;
                }
            }
        }
        return sum;
    }

    public static void main(String[] args) {
        Shape[] shapes = { new Circle(1.0), new Rect(2.0, 3.0), Shape.unit() };
        System.out.println(total(shapes));
        System.out.println(shapes[0].describe());
    }
}
