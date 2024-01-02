const header = document.querySelector(".wiki_text_block>table>tbody");

if (header) {
    current_row = header.insertRow();
    user_1 = current_row.insertCell();
    score_1 = current_row.insertCell();
    user_1.innerHTML = '<span class="tiny-user">Nume 1</span>';
    score_1.innerHTML = '90 puncte'

    user_2 = current_row.insertCell();
    score_2 = current_row.insertCell();


    const text = header.innerHTML;

    console.log(text);
}
