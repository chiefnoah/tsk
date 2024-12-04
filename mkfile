
index.html: readme
	pandoc -s -f markdown -t html5 -o $target $prereq

deploy:V: index.html readme
	rsync -rv $prereq pgs.sh:/tsk/

