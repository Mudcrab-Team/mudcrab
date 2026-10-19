// Args: <out.txt> <hex rva>... Dumps every instruction of the function
// containing each RVA as "<func-rva>+<instr-rva>: <text>" lines.
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.Instruction;
import ghidra.program.model.listing.InstructionIterator;
import java.io.PrintWriter;

public class DisasmAt extends GhidraScript {
    @Override
    public void run() throws Exception {
        String[] args = getScriptArgs();
        long base = currentProgram.getImageBase().getOffset();
        try (PrintWriter out = new PrintWriter(args[0], "UTF-8")) {
            for (int i = 1; i < args.length; i++) {
                long rva = Long.parseUnsignedLong(args[i], 16);
                Address at = toAddr(base + rva);
                Function f = getFunctionContaining(at);
                if (f == null) {
                    out.println("// no function at " + args[i]);
                    continue;
                }
                long fRva = f.getEntryPoint().getOffset() - base;
                out.println("// func " + Long.toHexString(fRva) + " body ["
                    + Long.toHexString(f.getBody().getMinAddress().getOffset() - base)
                    + ", " + Long.toHexString(f.getBody().getMaxAddress().getOffset() - base) + "]");
                InstructionIterator it = currentProgram.getListing().getInstructions(f.getBody(), true);
                while (it.hasNext()) {
                    Instruction ins = it.next();
                    long off = ins.getAddress().getOffset() - base - fRva;
                    out.printf("  %x+%02x: %s%n", fRva, off, ins.toString());
                }
            }
        }
    }
}
