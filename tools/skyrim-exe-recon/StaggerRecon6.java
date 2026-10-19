import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.address.AddressSpace;
import java.io.File;
import java.io.PrintWriter;
public class StaggerRecon6 extends GhidraScript {
    public void run() throws Exception {
        AddressSpace space = currentProgram.getAddressFactory().getDefaultAddressSpace();
        try (PrintWriter w = new PrintWriter(new File("/tmp/opencode/re-stagger/xs_1420a7ee0.tsv"), "UTF-8")) {
            for (var r : currentProgram.getReferenceManager().getReferencesTo(space.getAddress(0x1420a7ee0L))) {
                var f = getFunctionContaining(r.getFromAddress());
                w.println(r.getFromAddress() + "\t" + (f != null ? f.getEntryPoint().toString() : "NONE"));
            }
        }
        println("DONE6");
    }
}
