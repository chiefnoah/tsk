
index.html: readme
	pandoc -s -f markdown -t html5 -o $target $prereq

