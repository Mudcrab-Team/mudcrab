// Pass 4: walk the runtime settings name band 0x1420a0000-0x1420b0000 at
// stride 0x18 and dump name-pointer + string for every plausible struct;
// disassemble the three helper addresses Ghidra had no functions for.
// Args: <out_dir>
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.address.AddressSpace;
import java.io.File;
import java.io.PrintWriter;

public class StaggerRecon4 extends GhidraScript {
    @Override
    public void run() throws Exception {
        String outDir = getScriptArgs()[0];
        File out = new File(outDir);
        out.mkdirs();
        long base = currentProgram.getImageBase().getOffset();
        AddressSpace space = currentProgram.getAddressFactory().getDefaultAddressSpace();

        long lo = 0x1420a0000L, hi = 0x1420b0000L;
        try (PrintWriter w = new PrintWriter(new File(out, "band-names.tsv"), "UTF-8")) {
            for (long va = lo; va < hi; va += 0x18) {
                String name;
                int val;
                try {
                    long q = currentProgram.getMemory().getLong(space.getAddress(va));
                    name = "<no ptr>";
                    if (q > 0x140000000L && q < 0x148000000L) {
                        byte[] b = new byte[80];
                        currentProgram.getMemory().getBytes(space.getAddress(q), b);
                        int len = 0;
                        while (len < b.length && b[len] != 0) len++;
                        if (len > 1 && len < 80) name = new String(b, 0, len, "UTF-8");
                    }
                    val = currentProgram.getMemory().getInt(space.getAddress(va + 0x10));
                } catch (Exception e) {
                    continue;
                }
                float f = Float.intBitsToFloat(val);
                w.printf("%x\t%s\t%08x\t%f%n", va, name, val, f);
            }
        }

        try (PrintWriter w = new PrintWriter(new File(out, "helpers.asm"), "UTF-8")) {
            for (long r : new long[]{0x6742a0L, 0x6ca140L, 0x5535a0L}) {
                w.println("=== " + Long.toHexString(r) + " ===");
                var it = currentProgram.getListing().getInstructions(space.getAddress(base + r), true);
                int n = 0;
                while (it.hasNext() && n < 60) {
                    var ins = it.next();
                    w.println(Long.toHexString(ins.getAddress().getOffset() - base) + "  " + ins);
                    n++;
                }
            }
        }
        println("STAGGER-RECON4-DONE");
    }
}
