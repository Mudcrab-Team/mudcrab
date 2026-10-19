// Pass 2: disassembly around the stagger-entry call sites in 0x6ca640, the
// remaining helper decompiles, and the pool/slot value dump.
// Args: <out_dir>
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.AddressSpace;
import ghidra.program.model.listing.Function;
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileResults;
import java.io.File;
import java.io.PrintWriter;

public class StaggerRecon2 extends GhidraScript {
    @Override
    public void run() throws Exception {
        String outDir = getScriptArgs()[0];
        File out = new File(outDir);
        out.mkdirs();
        long base = currentProgram.getImageBase().getOffset();
        AddressSpace space = currentProgram.getAddressFactory().getDefaultAddressSpace();

        try (PrintWriter w = new PrintWriter(new File(out, "st-6ca640-callsites.asm"), "UTF-8")) {
            var listing = currentProgram.getListing();
            var it = listing.getInstructions(space.getAddress(base + 0x6cab80), true);
            int n = 0;
            while (it.hasNext() && n < 400) {
                var ins = it.next();
                long a = ins.getAddress().getOffset();
                if (a > base + 0x6cb1a0) break;
                w.println(Long.toHexString(a - base) + "  " + ins);
                n++;
            }
        }

        String[] fns = {"6cba10", "6742a0", "687c20", "6ca140", "28c140", "21b950",
                        "720660", "679d00", "69fb20", "5e2d30", "643490", "5535a0"};
        DecompInterface di = new DecompInterface();
        di.openProgram(currentProgram);
        for (String r : fns) {
            Function f = getFunctionContaining(space.getAddress(base + Long.parseUnsignedLong(r, 16)));
            if (f == null) { println("nofn " + r); continue; }
            DecompileResults res = di.decompileFunction(f, 240, monitor);
            try (PrintWriter w = new PrintWriter(new File(out, r + ".c"), "UTF-8")) {
                w.println(res.decompileCompleted() ? res.getDecompiledFunction().getC()
                                                   : "// failed: " + res.getErrorMessage());
            }
        }
        di.dispose();

        String[] memVAs = {"141b5be28", "14191c7ac", "1420a6098", "1420a60b0", "1420a6128",
                           "1420a6140", "1420a6158", "1420a6170", "1420a6188", "1420a61a0",
                           "141b5be2c"};
        try (PrintWriter w = new PrintWriter(new File(out, "mem2.tsv"), "UTF-8")) {
            for (String s : memVAs) {
                long va = Long.parseUnsignedLong(s, 16);
                try {
                    int u = currentProgram.getMemory().getInt(space.getAddress(va));
                    float f = Float.intBitsToFloat(u);
                    w.printf("%s\tu=%08x\tf=%f%n", s, u, f);
                } catch (Exception e) {
                    w.printf("%s\tERR\t%s%n", s, e.getMessage());
                }
            }
        }
        println("STAGGER-RECON2-DONE");
    }
}
