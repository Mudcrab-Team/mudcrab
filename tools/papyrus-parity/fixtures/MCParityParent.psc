Scriptname MCParityParent extends MCParityGrand

String Function DefaultVsParentState()
    Return "parent.empty"
EndFunction

String Function VirtualLeaf()
    Return "parent.empty"
EndFunction

String Function ParentOnly()
    Return "parent.empty"
EndFunction

String Function GrandStateOverParentDefault()
    Return "parent.empty"
EndFunction

String Function CallVirtual()
    Return VirtualLeaf()
EndFunction

String Function Chain()
    Return "parent/" + Parent.Chain()
EndFunction

State Ready
    String Function DefaultVsParentState()
        Return "parent.ready"
    EndFunction

    String Function ParentOnly()
        Return "parent.ready"
    EndFunction
EndState
