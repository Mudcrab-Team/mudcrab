Scriptname MCParityGrand extends Quest

String Function GrandOnly()
    Return "grand.empty"
EndFunction

String Function Chain()
    Return "grand"
EndFunction

String Function GrandStateOverParentDefault()
    Return "grand.empty"
EndFunction

State Ready
    String Function GrandStateOverParentDefault()
        Return "grand.ready"
    EndFunction
EndState
