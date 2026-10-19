// Args: <out.txt> <hex va>... Prints u32/f32 at each VA from the loaded image.
import ghidra.app.script.GhidraScript;
import java.io.PrintWriter;

public class ReadMem extends GhidraScript {
    @Override
    public void run() throws Exception {
        String[] args = getScriptArgs();
        long base = currentProgram.getImageBase().getOffset();
        try (PrintWriter w = new PrintWriter(args[0], "UTF-8")) {
            for (int i = 1; i < args.length; i++) {
                long va = Long.parseUnsignedLong(args[i], 16);
                try {
                    int u = currentProgram.getMemory().getInt(toAddr(va));
                    float f = Float.intBitsToFloat(u);
                    w.printf("%x\t%08x\t%f%n", va, u, f);
                } catch (Exception e) {
                    w.printf("%x\tERR\t%s%n", va, e.getMessage());
                }
            }
        }
    }
}
